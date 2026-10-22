//! Smooth playback of the other player's position (BotW multiplayer).
//!
//! Measured 2026-10-20 (main PC watching the STRIX player walk):
//! - the friend's updates reach the mod irregularly (gaps of ~20 ms, but also 36-40 ms followed by a 4 ms pair: Wi-Fi
//!   bunching on the friend -> host leg; the server is request/response, so it forwards the timing unchanged);
//! - the mod's DLL only dead-reckons a short step from the latest update every 16 ms, so a late pair shows as a stall
//!   (14 % of frames) and then a double step (step size cv 0.60 vs 0.18 for the real motion): "10-25 FPS".
//!
//! Fix: a jitter buffer. Every distinct position the mod receives is a snapshot; they are played back at the sender's
//! steady rate a couple of snapshots behind the newest, interpolating in between. The playback rate is nudged to keep
//! the buffer depth around [`TARGET_DEPTH`], so the delay stays ~45 ms and never drifts. When the buffer runs dry
//! (the friend stopped, or a long gap), playback holds the last position instead of guessing.

use std::collections::VecDeque;

/// Snapshots kept behind the newest one (playback delay = this many sender intervals).
pub const TARGET_DEPTH: f64 = 2.5;
/// How strongly playback speed follows the buffer depth (per snapshot of difference).
pub const RATE_GAIN: f64 = 0.25;
/// Time constant (ms) of the buffer-depth average the rate is steered on.
pub const DEPTH_SMOOTH_MS: f64 = 300.0;
/// Expected sender interval when nothing has been measured yet (the BOTWM client sends every ~18 ms).
pub const DEFAULT_INTERVAL_MS: f64 = 18.5;
/// A gap this long means the friend stopped (or teleported/respawned): start over instead of interpolating across it.
pub const RESET_GAP_MS: f64 = 250.0;
/// A jump this far (metres) between two snapshots is a teleport: snap instead of gliding.
pub const TELEPORT_M: f32 = 8.0;

/// The jitter buffer for one remote player.
#[derive(Debug, Default)]
pub struct Smoother {
    /// Snapshots (arrival time ms, position), oldest first; index 0 is snapshot number `base`.
    snaps: VecDeque<(f64, [f32; 3])>,
    base: u64,
    /// Playback cursor in snapshot numbers (fractional).
    cursor: f64,
    /// Sender interval estimate (ms), from the arrival rate over many snapshots.
    interval: f64,
    last_tick: Option<f64>,
    /// Smoothed buffer depth (snapshots).
    depth_avg: f64,
    /// Times the buffer started over (gap or teleport); for diagnostics.
    pub resets: u64,
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn lerp(a: [f32; 3], b: [f32; 3], k: f64) -> [f32; 3] {
    let k = k as f32;
    [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k]
}

impl Smoother {
    pub fn new() -> Self {
        Smoother { interval: DEFAULT_INTERVAL_MS, ..Default::default() }
    }

    fn newest(&self) -> f64 {
        self.base as f64 + self.snaps.len() as f64 - 1.0
    }

    fn reset(&mut self, t: f64, p: [f32; 3]) {
        self.resets += 1;
        self.snaps.clear();
        self.snaps.push_back((t, p));
        self.base = 0;
        self.cursor = 0.0;
    }

    /// A new position arrived at `t_ms` (call only when it differs from the previous one).
    pub fn push(&mut self, t_ms: f64, p: [f32; 3]) {
        match self.snaps.back() {
            None => return self.reset(t_ms, p),
            Some(&(lt, lp)) if t_ms - lt > RESET_GAP_MS || dist(lp, p) > TELEPORT_M => return self.reset(t_ms, p),
            _ => {}
        }
        // The buffer was idle (holding the last snapshot): restart playback from it, so a fresh walk starts smoothly.
        if self.cursor >= self.newest() {
            self.cursor = self.newest();
        }
        self.snaps.push_back((t_ms, p));
        // Interval from the arrival rate over the last few dozen snapshots (robust to single late/bunched ones).
        if self.snaps.len() >= 8 {
            let (t0, _) = self.snaps[self.snaps.len().saturating_sub(32)];
            let n = self.snaps.len().min(32) - 1;
            self.interval = ((t_ms - t0) / n as f64).clamp(8.0, 60.0);
        }
        // Drop snapshots the cursor has passed.
        while self.snaps.len() > 2 && (self.base as f64 + 1.0) < self.cursor.floor() {
            self.snaps.pop_front();
            self.base += 1;
        }
    }

    /// The position to show at `t_ms`, advancing playback. `None` before the first snapshot.
    pub fn sample(&mut self, t_ms: f64) -> Option<[f32; 3]> {
        let dt = self.last_tick.map_or(0.0, |l| (t_ms - l).max(0.0));
        self.last_tick = Some(t_ms);
        if self.snaps.is_empty() {
            return None;
        }
        let newest = self.newest();
        let depth = newest - self.cursor;
        // Steer the rate to hold the target depth, on a smoothed depth (it saw-tooths by one snapshot per arrival, and
        // steering on the raw value made the speed wobble: cv 0.25). An empty buffer pauses so it can refill: a weak
        // nudge with no pause (first try) kept it nearly empty on real arrivals (median depth 0.7), replaying the stalls.
        let k = (dt / DEPTH_SMOOTH_MS).min(1.0);
        self.depth_avg += (depth - self.depth_avg) * k;
        let rate = if depth <= 0.02 { 0.0 } else { (1.0 + RATE_GAIN * (self.depth_avg - TARGET_DEPTH)).clamp(0.5, 1.5) };
        // A fresh stream (cursor far behind) is not played from far back: jump to the target depth.
        if depth > TARGET_DEPTH + 4.0 {
            self.cursor = newest - TARGET_DEPTH;
        }
        self.cursor = (self.cursor + dt / self.interval * rate).min(newest);
        let i = (self.cursor.floor() as u64).saturating_sub(self.base) as usize;
        let k = self.cursor.fract();
        let a = self.snaps[i.min(self.snaps.len() - 1)].1;
        let b = self.snaps[(i + 1).min(self.snaps.len() - 1)].1;
        Some(lerp(a, b, k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sender moves 0.093 m per 18.4 ms along x (like the measured walk); arrivals bunch: every 3rd update is 18 ms
    /// late and arrives together with the next one.
    fn walk(n: usize) -> Vec<(f64, [f32; 3])> {
        (0..n)
            .map(|i| {
                let sent = i as f64 * 18.4;
                let late = if i % 3 == 1 { 17.0 } else { 0.0 };
                (sent + 5.0 + late, [i as f32 * 0.093, 0.0, 0.0])
            })
            .collect()
    }

    /// Per-frame (16.67 ms) steps of a position series.
    fn steps(frames: &[[f32; 3]]) -> Vec<f32> {
        frames.windows(2).map(|w| dist(w[0], w[1])).collect()
    }

    fn cv(v: &[f32]) -> f32 {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt() / m
    }

    #[test]
    fn bunched_updates_play_back_evenly() {
        let input = walk(200);
        let mut s = Smoother::new();
        // Raw: what the DLL shows = the latest arrived position at each frame.
        let (mut raw, mut smooth) = (Vec::new(), Vec::new());
        let mut next = 0;
        for f in 0..200 {
            let t = f as f64 * 16.667;
            while next < input.len() && input[next].0 <= t {
                s.push(input[next].0, input[next].1);
                next += 1;
            }
            if next > 0 {
                raw.push(input[next - 1].1);
            }
            if let Some(p) = s.sample(t) {
                smooth.push(p);
            }
        }
        // Skip the start-up and compare the steady part.
        let (r, m) = (steps(&raw[40..180]), steps(&smooth[40..180]));
        let stalls = |v: &[f32]| v.iter().filter(|x| **x < 0.005).count();
        println!("raw cv {:.2} stalls {} | smooth cv {:.2} stalls {}", cv(&r), stalls(&r), cv(&m), stalls(&m));
        assert!(stalls(&r) > 10, "the raw stream stalls");
        assert_eq!(stalls(&m), 0, "smooth playback never stalls while walking");
        assert!(cv(&m) < 0.1, "smooth steps are even: cv {}", cv(&m));
        // And it never trails the newest arrived position by more than ~TARGET_DEPTH snapshots (2.5 * 0.093 m).
        let lag = raw[180][0] - smooth[180][0];

        assert!(lag > -0.05 && lag < 0.45, "lag {lag} m");
    }

    /// The live pattern: an update every 40 ms, sampled every 1.6 ms. Playback must move between updates.
    #[test]
    fn moves_between_updates_at_the_live_rate() {
        let mut s = Smoother::new();
        let (mut changes, mut last) = (0, None);
        let mut t = 0.0;
        for i in 0..60 {
            s.push(i as f64 * 40.0, [i as f32 * 0.22, 0.0, 0.0]);
            while t < (i + 1) as f64 * 40.0 {
                let p = s.sample(t);
                if p != last {
                    changes += 1;
                    last = p;
                }
                t += 1.6;
            }
        }
        println!("changes {changes} over 60 updates; interval {:.1}; resets {}", s.interval, s.resets);
        assert!(changes > 600, "only {changes} output changes");
    }

    #[test]
    fn stops_where_the_friend_stopped_and_snaps_on_teleport() {
        let mut s = Smoother::new();
        for (t, p) in walk(30) {
            s.push(t, p);
        }
        let last = walk(30)[29].1;
        let mut p = None;
        for f in 0..120 {
            p = s.sample(560.0 + f as f64 * 16.667);
        }
        assert!((p.unwrap()[0] - last[0]).abs() < 1e-4, "ends exactly on the last position: {p:?}");
        // A teleport far away snaps immediately.
        s.push(3000.0, [500.0, 10.0, 0.0]);
        assert_eq!(s.sample(3001.0), Some([500.0, 10.0, 0.0]));
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// Replays a recorded arrival log (`utc_ms,x,y,z`, from docs/botwmp-gameplay/dll-players.ps1) through the smoother,
    /// sampling at 1 kHz, and prints how often the output changes.
    /// `BOTW_ARRIVALS=<csv> cargo test -p privcord-games --lib replay_arrivals -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn replay_arrivals() {
        let path = std::env::var("BOTW_ARRIVALS").expect("set BOTW_ARRIVALS");
        let rows: Vec<(f64, [f32; 3])> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .skip(1)
            .filter_map(|l| {
                let v: Vec<f64> = l.split(',').filter_map(|x| x.parse().ok()).collect();
                (v.len() == 4).then(|| (v[0], [v[1] as f32, v[2] as f32, v[3] as f32]))
            })
            .collect();
        let t0 = rows[0].0;
        let mut s = Smoother::new();
        let (mut i, mut changes, mut last) = (0, 0, None);
        let mut t = t0;
        let end = rows.last().unwrap().0;
        let mut depths = Vec::new();
        while t < end {
            while i < rows.len() && rows[i].0 <= t {
                s.push(rows[i].0 - t0, rows[i].1);
                i += 1;
            }
            let p = s.sample(t - t0);
            if p != last {
                changes += 1;
                last = p;
            }
            depths.push(s.newest() - s.cursor);
            t += 1.0;
        }
        depths.sort_by(f64::total_cmp);
        println!(
            "{} arrivals, {changes} output changes over {:.0} ms; interval {:.1} ms; depth p10 {:.2} p50 {:.2} p90 {:.2}",
            rows.len(),
            end - t0,
            s.interval,
            depths[depths.len() / 10],
            depths[depths.len() / 2],
            depths[depths.len() * 9 / 10]
        );
    }
}

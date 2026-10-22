//! Shared enemies for BotW multiplayer, phase 1: the same enemy at the same place on every screen.
//!
//! BOTWM 1.0.4 only syncs enemy health, so each game runs its own copy of every enemy. Here every map-placed enemy
//! (stable HashId, the same in both games) that is near two or more players gets one **owner**: the player whose Link
//! is nearest runs it with the game's own AI, so it targets that player's real Link. Every other game turns its copy
//! into a **puppet**: the PrivCordEnemies patch skips the copy's AI (the same skip the game already takes for
//! `CalcAISkipOnPhysPause`), and the sidecar writes the owner's position and rotation into it, played back through a
//! small jitter buffer. Ownership moves to another player when they are clearly closer ([`HANDOVER_M`] for
//! [`HANDOVER_HOLD`]), and is released when the enemy dies, unloads or leaves the shared area ([`SCOPE_M`]).
//! Animation (P2) and hits (P3) are later phases; see `docs/BOTWM-SHARED-ENEMIES-PLAN.md`.
//!
//! Only the current owner changes an enemy's ownership (hand over or release), and a claim on an unowned enemy is
//! settled by epoch and then by name, so two games never both freeze the same enemy for longer than a message trip,
//! and a stopped or silent owner frees its puppets after [`OWNER_STALE`] (the patch's lease does the same in game if
//! PrivCord itself stops).
//!
//! This file is the pure part (rules, wire format, interpolation, relay); `botw_enemies_sidecar.rs` runs it against
//! Cemu. Made by Blackz.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// TCP port of the enemy relay on the host (tunnelled to friends like 5053).
pub const RELAY_PORT: u16 = 5054;
/// The patch group whose codecave holds the puppet table.
pub const PATCH_GROUP: &str = "BotW_PrivCordEnemies_V208";
/// "PCE2": the codecave block's magic (layout version 2, with AS requests).
pub const MAGIC: u32 = 0x5043_4532;
/// Block offsets: entries in use, AI calcs skipped, AS requests played, the table.
pub const OFF_COUNT: u32 = 4;
pub const OFF_SKIPS: u32 = 8;
pub const OFF_PLAYS: u32 = 16;
pub const OFF_TABLE: u32 = 32;
pub const TABLE_LEN: usize = 64;
/// One table entry: HashId +0, lease +4, AS request no. +8, last played no. +12 (the patch writes it),
/// `sead::SafeString` {char* +16 (points at +24), vtable +20}, AS name +24 (24 bytes, zero-terminated).
pub const ENTRY_LEN: u32 = 48;
pub const ENTRY_REQ: u32 = 8;
pub const ENTRY_DONE: u32 = 12;
pub const ENTRY_NAME: u32 = 24;
/// `sead::SafeString`'s vtable in v208 (what the game's own callers of the AS request put there).
pub const SAFESTRING_VTABLE: u32 = 0x1026_3910;
/// Lease per puppet, in skipped AI updates (the game updates an enemy's AI every 1-3 frames). Refilled every
/// [`TABLE_REFRESH`]; if PrivCord stops, the enemy's AI comes back within ~0.5-2 s.
pub const LEASE: u32 = 40;
pub const TABLE_REFRESH: Duration = Duration::from_millis(50);

/// Enemies are shared only while they are this close to two or more players (metres).
pub const SCOPE_M: f32 = 150.0;
/// Another player takes over an enemy once they are this much closer to it than the owner...
pub const HANDOVER_M: f32 = 20.0;
/// ...for this long (no ping-pong when both stand about equally far).
pub const HANDOVER_HOLD: Duration = Duration::from_millis(500);
/// A player we haven't heard from for this long is gone (frames come ~60 times a second).
pub const PEER_STALE: Duration = Duration::from_millis(1500);
/// A puppet whose owner sent nothing for this long runs its own AI again.
pub const OWNER_STALE: Duration = Duration::from_secs(1);
/// If nobody claimed a shared enemy for this long (each side thinks the other is nearer, from slightly different
/// copies), the player with the smallest name claims it.
pub const CLAIM_FALLBACK: Duration = Duration::from_secs(2);
/// Puppets play the owner's motion this far behind the owner's clock: the 90th percentile of how late snapshots
/// arrive (beyond the fastest trip) plus one snapshot interval, kept between these bounds. On a LAN that is ~25 ms;
/// a relayed link with more jitter gets more delay instead of stutter. Before any measurement: [`INTERP_DELAY_MS`].
pub const INTERP_DELAY_MS: f64 = 50.0;
pub const MIN_DELAY_MS: f64 = 25.0;
pub const MAX_DELAY_MS: f64 = 200.0;
/// A new puppet glides from where its own copy stood to the owner's enemy over this long (they drifted apart while
/// both ran their own AI), instead of snapping.
pub const BLEND_IN_MS: f64 = 400.0;
/// A jump this large between two snapshots is a teleport: snap instead of gliding.
pub const TELEPORT_M: f32 = 8.0;
/// Largest frame the relay forwards.
pub const MAX_FRAME: usize = 64 * 1024;
/// An enemy's epoch may only move this far ahead per message (a peer can't jump it out of reach of the others).
pub const MAX_EPOCH_STEP: u32 = 16;
/// Players tracked at once (more is ignored: a session has a handful).
pub const MAX_PEERS: usize = 16;

/// Actor profiles that count as enemies (the same rule as the co-op difficulty pack).
pub const ENEMY_PROFILES: [&str; 7] = ["Enemy", "GiantEnemy", "Guardian", "Sandworm", "LastBoss", "SiteBoss", "GelEnemy"];

/// Where an enemy is and what it is doing: position, rotation (unit quaternion x, y, z, w), velocity (metres per
/// frame, as the actor stores it; carried for later phases) and its current AS (animation sequence, e.g. `Run`,
/// `Notice`, `JumpAttack`), which also tells the state: alert, searching, attacking, stunned, knocked down, dying.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub vel: [f32; 3],
    pub anim: Anim,
}

/// An AS name (slot 0, the body), at most [`Anim::MAX`] bytes; empty = unknown.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Anim(pub [u8; 24]);

impl Anim {
    /// Longest name kept (the patch's buffer is 24 bytes with the terminating zero).
    pub const MAX: usize = 23;

    pub fn new(name: &str) -> Anim {
        let mut b = [0u8; 24];
        let mut end = name.len().min(Self::MAX);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        b[..end].copy_from_slice(&name.as_bytes()[..end]);
        Anim(b)
    }

    /// An AS name from another player: only plain names (letters, digits, `_`, as all of BotW's are) that fit,
    /// anything else becomes "unknown" and is never sent to the game. (The game also ignores names the enemy's AS
    /// list doesn't have.)
    pub fn from_peer(name: &str) -> Anim {
        let plain = name.len() <= Self::MAX && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if plain { Anim::new(name) } else { Anim::default() }
    }

    pub fn as_str(&self) -> &str {
        let end = self.0.iter().position(|&c| c == 0).unwrap_or(24);
        std::str::from_utf8(&self.0[..end]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.0[0] == 0
    }
}

impl std::fmt::Debug for Anim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

/// Rotation part of a row-major 3x3 matrix (the actor's Matrix34 rows) as a unit quaternion.
pub fn quat_from_rows(m: [[f32; 3]; 3]) -> [f32; 4] {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, 0.25 * s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [0.25 * s, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, 0.25 * s, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, 0.25 * s, (m[1][0] - m[0][1]) / s]
    };
    normalize(q)
}

/// Row-major 3x3 rotation matrix of a unit quaternion (x, y, z, w).
pub fn rows_from_quat(q: [f32; 4]) -> [[f32; 3]; 3] {
    let [x, y, z, w] = normalize(q);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

fn normalize(q: [f32; 4]) -> [f32; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n < 1e-6 || !n.is_finite() { [0.0, 0.0, 0.0, 1.0] } else { q.map(|v| v / n) }
}

pub fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn lerp3(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k]
}

/// Normalised linear quaternion interpolation along the shorter arc.
fn nlerp(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let b = if dot < 0.0 { b.map(|v| -v) } else { b };
    normalize([0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * k))
}

/// Blend from `from` to `to` (k = 0..1).
pub fn blend(from: &Pose, to: &Pose, k: f32) -> Pose {
    lerp_pose(from, to, k.clamp(0.0, 1.0))
}

/// Between two snapshots: motion is interpolated, the AS is the earlier snapshot's until the later one is reached.
fn lerp_pose(a: &Pose, b: &Pose, k: f32) -> Pose {
    Pose { pos: lerp3(a.pos, b.pos, k), rot: nlerp(a.rot, b.rot, k), vel: lerp3(a.vel, b.vel, k), anim: if k >= 1.0 { b.anim } else { a.anim } }
}

// ---------------------------------------------------------------------------------------------------------------
// Wire format: one frame per sender tick, big-endian, length-prefixed on the relay ([`write_frame`] / [`read_frame`]).

/// An enemy the sender owns, with its state this tick.
#[derive(Clone, Debug, PartialEq)]
pub struct Owned {
    pub hash: u32,
    pub epoch: u32,
    pub pose: Pose,
    pub life: i32,
}

/// What a player's sidecar sends every tick.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Frame {
    /// The sender's PrivCord display name (unique within a session).
    pub from: String,
    /// The sender's clock (ms since its sidecar started); puppets interpolate on it.
    pub t_ms: u32,
    /// The sender's Link, if in game.
    pub link: Option<[f32; 3]>,
    /// HashIds of the live enemies loaded in the sender's game.
    pub loaded: Vec<u32>,
    pub owned: Vec<Owned>,
    /// (hash, new epoch, new owner): the sender gives an enemy to another player.
    pub handovers: Vec<(u32, u32, String)>,
    /// (hash, new epoch, died): the sender stops owning an enemy; everyone runs their own copy again. `died`: it died
    /// in the owner's game, so the other copies die too.
    pub releases: Vec<(u32, u32, bool)>,
}

/// 2: AS name per owned enemy and the `died` flag on releases (phase 2).
const WIRE_VERSION: u8 = 2;
const MAX_LIST: usize = 1024;

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits).filter(|v| v.is_finite())
    }
    fn v3(&mut self) -> Option<[f32; 3]> {
        Some([self.f32()?, self.f32()?, self.f32()?])
    }
    fn name(&mut self) -> Option<String> {
        let n = self.u8()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn count(&mut self) -> Option<usize> {
        Some(self.u16()? as usize).filter(|&n| n <= MAX_LIST)
    }
}

fn put_name(out: &mut Vec<u8>, s: &str) {
    // Names are display names; cut at a char boundary to fit the length byte.
    let mut end = s.len().min(255);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push(end as u8);
    out.extend_from_slice(&s.as_bytes()[..end]);
}

fn put_v3(out: &mut Vec<u8>, v: [f32; 3]) {
    for x in v {
        out.extend_from_slice(&x.to_be_bytes());
    }
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(32 + self.loaded.len() * 4 + self.owned.len() * 44);
        o.push(WIRE_VERSION);
        put_name(&mut o, &self.from);
        o.extend_from_slice(&self.t_ms.to_be_bytes());
        match self.link {
            Some(p) => {
                o.push(1);
                put_v3(&mut o, p);
            }
            None => o.push(0),
        }
        let loaded = &self.loaded[..self.loaded.len().min(MAX_LIST)];
        o.extend_from_slice(&(loaded.len() as u16).to_be_bytes());
        for h in loaded {
            o.extend_from_slice(&h.to_be_bytes());
        }
        let owned = &self.owned[..self.owned.len().min(MAX_LIST)];
        o.extend_from_slice(&(owned.len() as u16).to_be_bytes());
        for e in owned {
            o.extend_from_slice(&e.hash.to_be_bytes());
            o.extend_from_slice(&e.epoch.to_be_bytes());
            put_v3(&mut o, e.pose.pos);
            for c in normalize(e.pose.rot) {
                o.extend_from_slice(&((c * 32767.0).round() as i16).to_be_bytes());
            }
            put_v3(&mut o, e.pose.vel);
            o.extend_from_slice(&e.life.to_be_bytes());
            put_name(&mut o, e.pose.anim.as_str());
        }
        let hand = &self.handovers[..self.handovers.len().min(MAX_LIST)];
        o.extend_from_slice(&(hand.len() as u16).to_be_bytes());
        for (h, ep, to) in hand {
            o.extend_from_slice(&h.to_be_bytes());
            o.extend_from_slice(&ep.to_be_bytes());
            put_name(&mut o, to);
        }
        let rel = &self.releases[..self.releases.len().min(MAX_LIST)];
        o.extend_from_slice(&(rel.len() as u16).to_be_bytes());
        for (h, ep, died) in rel {
            o.extend_from_slice(&h.to_be_bytes());
            o.extend_from_slice(&ep.to_be_bytes());
            o.push(u8::from(*died));
        }
        o
    }

    pub fn decode(b: &[u8]) -> Option<Frame> {
        let mut r = Rd(b);
        if r.u8()? != WIRE_VERSION {
            return None;
        }
        let from = r.name()?;
        let t_ms = r.u32()?;
        let link = match r.u8()? {
            0 => None,
            _ => Some(r.v3()?),
        };
        let loaded = (0..r.count()?).map(|_| r.u32()).collect::<Option<Vec<_>>>()?;
        let owned = (0..r.count()?)
            .map(|_| {
                let hash = r.u32()?;
                let epoch = r.u32()?;
                let pos = r.v3()?;
                let mut rot = [0f32; 4];
                for c in &mut rot {
                    *c = r.u16()? as i16 as f32 / 32767.0;
                }
                let vel = r.v3()?;
                let life = r.u32()? as i32;
                let anim = Anim::from_peer(&r.name()?);
                Some(Owned { hash, epoch, pose: Pose { pos, rot: normalize(rot), vel, anim }, life })
            })
            .collect::<Option<Vec<_>>>()?;
        let handovers = (0..r.count()?).map(|_| Some((r.u32()?, r.u32()?, r.name()?))).collect::<Option<Vec<_>>>()?;
        let releases = (0..r.count()?).map(|_| Some((r.u32()?, r.u32()?, r.u8()? != 0))).collect::<Option<Vec<_>>>()?;
        Some(Frame { from, t_ms, link, loaded, owned, handovers, releases })
    }
}

pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut b = Vec::with_capacity(4 + payload.len());
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    b.extend_from_slice(payload);
    w.write_all(&b).await
}

/// One length-prefixed frame; `Ok(None)` at a clean end of stream. Oversized frames are an error (the peer is
/// broken or hostile; drop it).
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "enemy frame too large"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).await?;
    Ok(Some(b))
}

/// The host's relay: every frame from one client goes to every other client. A slow client loses frames rather than
/// holding the others up (frames are state, the next one replaces it).
pub async fn run_relay(listener: tokio::net::TcpListener) {
    let (tx, _) = tokio::sync::broadcast::channel::<(u64, std::sync::Arc<Vec<u8>>)>(512);
    let mut next = 0u64;
    while let Ok((sock, _)) = listener.accept().await {
        let _ = sock.set_nodelay(true);
        next += 1;
        let id = next;
        let tx = tx.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let (mut r, mut w) = sock.into_split();
            let reader = async {
                while let Ok(Some(f)) = read_frame(&mut r).await {
                    let _ = tx.send((id, std::sync::Arc::new(f)));
                }
            };
            let writer = async {
                loop {
                    match rx.recv().await {
                        Ok((from, f)) if from != id => {
                            if write_frame(&mut w, &f).await.is_err() {
                                break;
                            }
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    }
                }
            };
            tokio::select! { _ = reader => {}, _ = writer => {} }
        });
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Puppet playback: snapshots on the owner's clock, played a little behind (see [`INTERP_DELAY_MS`]).

/// The owner's recent snapshots of one enemy.
#[derive(Debug, Default)]
pub struct Track {
    /// (sender ms, pose), oldest first.
    snaps: VecDeque<(f64, Pose)>,
    /// local ms - sender ms, as low as seen (= clock difference + the fastest trip), creeping up slowly so clock
    /// drift can't leave it behind.
    offset: Option<f64>,
    last_local: f64,
    /// Shown while no snapshot has arrived (after a hand-over: where the previous owner left it).
    hold: Option<Pose>,
    /// How late recent snapshots arrived beyond the fastest one (ms), for the playback delay.
    late: VecDeque<f64>,
    /// Target delay from `late`, and the delay in use, which moves toward it at most 10 % of real time (so playback
    /// speeds up or slows down a little instead of jumping).
    delay_target: Option<f64>,
    /// The delay in use at local time `delay_base.1`.
    delay_base: Option<(f64, f64)>,
}

impl Track {
    pub fn clear(&mut self) {
        *self = Track::default();
    }

    /// Start over (a new sender), showing `pose` until its snapshots arrive.
    pub fn restart(&mut self, pose: Option<Pose>) {
        *self = Track { hold: pose, ..Track::default() };
    }

    pub fn push(&mut self, local_ms: f64, sender_ms: u32, pose: Pose) {
        let s = sender_ms as f64;
        let o = local_ms - s;
        match self.offset {
            // A restarted sender (its clock jumped): start over.
            Some(off) if (o - off).abs() > 5000.0 => {
                self.snaps.clear();
                self.offset = Some(o);
            }
            Some(off) if o < off => self.offset = Some(o),
            Some(off) => self.offset = Some(off + (o - off) * ((local_ms - self.last_local).max(0.0) / 20_000.0).min(1.0)),
            None => self.offset = Some(o),
        }
        self.last_local = local_ms;
        if let Some(off) = self.offset {
            self.late.push_back(o - off);
            if self.late.len() > 120 {
                self.late.pop_front();
            }
            if self.late.len() >= 20 && self.late.len() % 10 == 0 {
                let mut v: Vec<f64> = self.late.iter().copied().collect();
                v.sort_by(f64::total_cmp);
                let p90 = v[v.len() * 9 / 10];
                let interval = self.snaps.len().checked_sub(1).filter(|&n| n >= 4).map_or(16.7, |n| {
                    (self.snaps.back().unwrap().0 - self.snaps.front().unwrap().0) / n as f64
                });
                self.delay_target = Some((p90 + interval).clamp(MIN_DELAY_MS, MAX_DELAY_MS));
            }
        }
        if self.delay_target.is_some() {
            self.delay_base = Some((self.delay_at(local_ms), local_ms));
        }
        if self.snaps.back().is_some_and(|&(t, _)| t >= s) {
            return; // duplicate or out of order
        }
        self.snaps.push_back((s, pose));
        // Keep ~1.5 s for playback and for measuring the sync error afterwards.
        while self.snaps.len() > 2 && self.snaps.front().is_some_and(|&(t, _)| t < s - 1500.0) {
            self.snaps.pop_front();
        }
    }

    /// The owner's pose at sender time `s` (held at the ends of the buffer).
    pub fn at(&self, s: f64) -> Option<Pose> {
        let i = self.snaps.iter().position(|&(t, _)| t > s);
        match i {
            None => self.snaps.back().map(|&(_, p)| p).or(self.hold),
            Some(0) => self.snaps.front().map(|&(_, p)| p),
            Some(i) => {
                let (ta, a) = self.snaps[i - 1];
                let (tb, b) = self.snaps[i];
                if dist(a.pos, b.pos) > TELEPORT_M {
                    return Some(b);
                }
                Some(lerp_pose(&a, &b, ((s - ta) / (tb - ta)) as f32))
            }
        }
    }

    /// What to show at local time `local_ms`.
    pub fn sample(&self, local_ms: f64) -> Option<Pose> {
        match self.sender_time(local_ms) {
            Some(s) => self.at(s - self.delay_at(local_ms)),
            None => self.hold,
        }
    }

    /// The current playback delay (ms).
    pub fn delay_ms(&self) -> f64 {
        self.delay_at(self.last_local)
    }

    /// The delay at local time `local_ms`: continuous in time, so playback never jumps.
    fn delay_at(&self, local_ms: f64) -> f64 {
        let (d, t) = self.delay_base.unwrap_or((INTERP_DELAY_MS, local_ms));
        match self.delay_target {
            Some(target) => {
                let step = 0.1 * (local_ms - t).max(0.0);
                d + (target - d).clamp(-step, step)
            }
            None => d,
        }
    }

    /// The owner's clock at local time `local_ms` (minus the fastest one-way trip).
    pub fn sender_time(&self, local_ms: f64) -> Option<f64> {
        Some(local_ms - self.offset?)
    }

    /// When (sender time, at or before `s`) the owner's enemy last switched to the AS `anim`.
    pub fn anim_started(&self, s: f64, anim: Anim) -> Option<f64> {
        let mut start = None;
        let mut prev: Option<Anim> = None;
        for &(t, p) in &self.snaps {
            if t > s {
                break;
            }
            if p.anim == anim && prev != Some(anim) {
                start = Some(t);
            }
            prev = Some(p.anim);
        }
        start
    }

    /// Whether snapshots up to sender time `s` have arrived (so [`Track::at`] interpolates instead of holding).
    pub fn covers(&self, s: f64) -> bool {
        self.snaps.back().is_some_and(|&(t, _)| t >= s) && self.snaps.front().is_some_and(|&(t, _)| t <= s)
    }

    pub fn newest(&self) -> Option<Pose> {
        self.snaps.back().map(|&(_, p)| p).or(self.hold)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Ownership.

/// This game's part in one enemy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Not shared: the game runs it as always.
    Local,
    /// We run it and send its state.
    Owner,
    /// Another player runs it; our copy is frozen and follows theirs.
    Puppet,
}

/// An enemy in this game this tick (only live, map-placed ones).
#[derive(Clone, Copy, Debug)]
pub struct Local {
    pub pose: Pose,
    pub life: i32,
}

/// A change worth logging.
#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// We took ownership of an unowned enemy.
    Claimed { hash: u32 },
    /// We gave it to `to`.
    HandedOver { hash: u32, to: String },
    /// We stopped owning it (dead, unloaded, or nobody else near).
    Released { hash: u32 },
    /// Our copy now follows `owner`'s.
    Puppet { hash: u32, owner: String },
    /// We became the owner by hand-over.
    TookOver { hash: u32 },
    /// Our copy runs on its own again (released, owner gone, or it died here).
    Freed { hash: u32 },
}

#[derive(Debug)]
struct Enemy {
    owner: Option<String>,
    epoch: u32,
    owner_seen: Instant,
    track: Track,
    /// Whose snapshots `track` holds (each player has their own clock).
    track_from: String,
    /// (player, since): another player has been [`HANDOVER_M`] closer since then.
    closer: Option<(String, Instant)>,
    /// Shared but unclaimed since.
    unclaimed: Option<Instant>,
    role: Role,
    /// The owner's latest life for it.
    owner_life: Option<i32>,
}

impl Enemy {
    fn new(now: Instant) -> Self {
        Enemy { owner: None, epoch: 0, owner_seen: now, track: Track::default(), track_from: String::new(), closer: None, unclaimed: None, role: Role::Local, owner_life: None }
    }
}

#[derive(Debug)]
struct Peer {
    link: Option<[f32; 3]>,
    loaded: HashSet<u32>,
    seen: Instant,
}

/// Output of one [`Sync::tick`].
#[derive(Debug, Default)]
pub struct Tick {
    pub frame: Frame,
    /// HashIds our game must freeze and drive from [`Sync::sample`].
    pub puppets: Vec<u32>,
    /// HashIds that died in their owner's game: our copy (no longer a puppet) must die too.
    pub kill: Vec<u32>,
    pub notices: Vec<Notice>,
}

/// One player's view of who runs which shared enemy.
#[derive(Debug)]
pub struct Sync {
    me: String,
    peers: HashMap<String, Peer>,
    enemies: HashMap<u32, Enemy>,
    /// HashIds loaded in our game at the last tick: frames about anything else are ignored, so a peer can't make us
    /// track enemies we don't have.
    loaded: HashSet<u32>,
    /// Released with "died", for the next tick's [`Tick::kill`].
    died: Vec<u32>,
}

impl Sync {
    pub fn new(me: impl Into<String>) -> Self {
        Sync { me: me.into(), peers: HashMap::new(), enemies: HashMap::new(), loaded: HashSet::new(), died: Vec::new() }
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    pub fn peers(&self) -> usize {
        self.peers.len()
    }

    pub fn role(&self, hash: u32) -> Role {
        self.enemies.get(&hash).map_or(Role::Local, |e| e.role)
    }

    pub fn track(&self, hash: u32) -> Option<&Track> {
        self.enemies.get(&hash).map(|e| &e.track)
    }

    /// The owner's latest life for a puppet (its copy's life is kept at or below it).
    pub fn owner_life(&self, hash: u32) -> Option<i32> {
        self.enemies.get(&hash).filter(|e| e.role == Role::Puppet).and_then(|e| e.owner_life)
    }

    /// The pose a puppet should show at local time `local_ms`.
    pub fn sample(&self, hash: u32, local_ms: f64) -> Option<Pose> {
        self.enemies.get(&hash).filter(|e| e.role == Role::Puppet).and_then(|e| e.track.sample(local_ms))
    }

    /// A frame from another player, received at `now` (`local_ms` on our clock).
    pub fn receive(&mut self, now: Instant, local_ms: f64, f: Frame) {
        if f.from == self.me || f.from.is_empty() {
            return;
        }
        if !self.peers.contains_key(&f.from) && self.peers.len() >= MAX_PEERS {
            return;
        }
        self.peers.insert(f.from.clone(), Peer { link: f.link, loaded: f.loaded.iter().copied().collect(), seen: now });
        // An epoch from a peer is believable only a few steps ahead of ours (any epoch for an enemy we know nothing of).
        let plausible = |e: &Enemy, epoch: u32| e.epoch == 0 || epoch <= e.epoch.saturating_add(MAX_EPOCH_STEP);
        for o in f.owned {
            if !self.loaded.contains(&o.hash) {
                continue;
            }
            let e = self.enemies.entry(o.hash).or_insert_with(|| Enemy::new(now));
            if !plausible(e, o.epoch) {
                continue;
            }
            let accept = o.epoch > e.epoch
                || (o.epoch == e.epoch
                    && match &e.owner {
                        None => true,
                        Some(w) if *w == f.from => true,
                        // Two claims for the same epoch: the smaller name wins, on every side.
                        Some(w) => f.from < *w,
                    });
            if !accept {
                continue;
            }
            if e.owner.as_deref() != Some(&f.from) {
                e.closer = None;
            }
            if e.track_from != f.from {
                let last = e.track.newest();
                e.track.restart(last);
                e.track_from = f.from.clone();
            }
            e.epoch = o.epoch;
            e.owner = Some(f.from.clone());
            e.owner_seen = now;
            e.owner_life = Some(o.life);
            e.track.push(local_ms, f.t_ms, o.pose);
        }
        // Hand-overs and releases count only from the enemy's current owner.
        for (hash, epoch, to) in f.handovers {
            let Some(e) = self.enemies.get_mut(&hash) else { continue };
            if e.owner.as_deref() == Some(f.from.as_str()) && epoch > e.epoch && plausible(e, epoch) {
                e.epoch = epoch;
                e.owner = Some(to);
                e.owner_seen = now;
                e.closer = None;
                // Keep the old owner's last snapshots: until the new owner's arrive, a puppet holds there.
            }
        }
        for (hash, epoch, died) in f.releases {
            if let Some(e) = self.enemies.get_mut(&hash) {
                if e.owner.as_deref() == Some(f.from.as_str()) && epoch > e.epoch && plausible(e, epoch) {
                    if died {
                        self.died.push(hash);
                    }
                    e.epoch = epoch;
                    e.owner = None;
                    e.owner_life = None;
                    e.unclaimed = None;
                }
            }
        }
    }

    /// Candidates to run `pos` besides us: fresh players within [`SCOPE_M`] who have the enemy loaded, nearest first.
    fn others_near(&self, hash: u32, pos: [f32; 3]) -> Vec<(String, f32)> {
        let mut v: Vec<(String, f32)> = self
            .peers
            .iter()
            .filter(|(_, p)| p.loaded.contains(&hash))
            .filter_map(|(n, p)| p.link.map(|l| (n.clone(), dist(l, pos))))
            .filter(|(_, d)| *d <= SCOPE_M)
            .collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// One tick at `now`: our Link and our live enemies in, the frame to send and the puppets to drive out.
    pub fn tick(&mut self, now: Instant, t_ms: u32, link: Option<[f32; 3]>, locals: &HashMap<u32, Local>) -> Tick {
        self.peers.retain(|_, p| now.duration_since(p.seen) < PEER_STALE);
        let mut out = Tick::default();
        out.kill = std::mem::take(&mut self.died).into_iter().filter(|h| locals.contains_key(h)).collect();
        out.frame.from = self.me.clone();
        out.frame.t_ms = t_ms;
        out.frame.link = link;
        out.frame.loaded = locals.iter().filter(|(_, l)| l.life > 0).map(|(&h, _)| h).collect();
        out.frame.loaded.sort_unstable();
        self.loaded = locals.keys().copied().collect();
        for &h in locals.keys() {
            self.enemies.entry(h).or_insert_with(|| Enemy::new(now));
        }
        let peers_alive: HashSet<String> = self.peers.keys().cloned().collect();
        let hashes: Vec<u32> = self.enemies.keys().copied().collect();
        for hash in hashes {
            let local = locals.get(&hash).filter(|l| l.life > 0).copied();
            let others = local.map(|l| self.others_near(hash, l.pose.pos)).unwrap_or_default();
            let my_d = match (link, local) {
                (Some(me), Some(l)) => dist(me, l.pose.pos),
                _ => f32::INFINITY,
            };
            let me = self.me.clone();
            let died = locals.get(&hash).is_some_and(|l| l.life <= 0);
            let e = self.enemies.get_mut(&hash).unwrap();
            let owner_fresh = |e: &Enemy| {
                e.owner.as_ref().is_some_and(|o| *o != me && peers_alive.contains(o)) && now.duration_since(e.owner_seen) < OWNER_STALE
            };
            let role = if e.owner.as_deref() == Some(me.as_str()) {
                match local {
                    // Dead or unloaded here, or nobody else near: let every copy run on its own again.
                    None => release(e, hash, died, &mut out),
                    Some(_) if others.is_empty() => release(e, hash, false, &mut out),
                    Some(l) => {
                        let (near, d) = &others[0];
                        let closer = *d + HANDOVER_M < my_d;
                        match &e.closer {
                            Some((n, _)) if !closer || n != near => e.closer = closer.then(|| (near.clone(), now)),
                            None if closer => e.closer = Some((near.clone(), now)),
                            _ => {}
                        }
                        let due = e.closer.as_ref().is_some_and(|(_, since)| now.duration_since(*since) >= HANDOVER_HOLD) || my_d > SCOPE_M;
                        if due {
                            e.epoch = e.epoch.saturating_add(1);
                            e.owner = Some(near.clone());
                            e.owner_seen = now;
                            e.closer = None;
                            out.frame.handovers.push((hash, e.epoch, near.clone()));
                            out.notices.push(Notice::HandedOver { hash, to: near.clone() });
                            // Hold where we left it until the new owner's snapshots arrive.
                            e.track.restart(Some(l.pose));
                            e.track_from = near.clone();
                            Role::Puppet
                        } else {
                            out.frame.owned.push(Owned { hash, epoch: e.epoch, pose: l.pose, life: l.life });
                            Role::Owner
                        }
                    }
                }
            } else if owner_fresh(e) && local.is_some() {
                Role::Puppet
            } else {
                // Nobody (alive) runs it for us: ours runs locally. Claim it if it's shared and we're nearest.
                match local {
                    Some(l) if my_d <= SCOPE_M && !others.is_empty() => {
                        let since = *e.unclaimed.get_or_insert(now);
                        let nearest = my_d <= others[0].1;
                        let fallback = now.duration_since(since) >= CLAIM_FALLBACK && others.iter().all(|(n, _)| me < *n);
                        if nearest || fallback {
                            e.epoch = e.epoch.saturating_add(1);
                            e.owner = Some(me.clone());
                            e.unclaimed = None;
                            e.closer = None;
                            e.track.clear();
                            e.track_from.clear();
                            out.frame.owned.push(Owned { hash, epoch: e.epoch, pose: l.pose, life: l.life });
                            out.notices.push(Notice::Claimed { hash });
                            Role::Owner
                        } else {
                            Role::Local
                        }
                    }
                    _ => {
                        e.unclaimed = None;
                        Role::Local
                    }
                }
            };
            if role != e.role {
                match (e.role, role) {
                    (_, Role::Puppet) => out.notices.push(Notice::Puppet { hash, owner: e.owner.clone().unwrap_or_default() }),
                    (Role::Puppet, Role::Owner) => out.notices.push(Notice::TookOver { hash }),
                    (Role::Puppet, Role::Local) => out.notices.push(Notice::Freed { hash }),
                    _ => {}
                }
                e.role = role;
            }
            if role == Role::Puppet {
                out.puppets.push(hash);
            }
        }
        // Forget enemies that are gone here and that nobody runs (bounded memory over a long session).
        self.enemies.retain(|h, e| locals.contains_key(h) || e.owner.is_some() && now.duration_since(e.owner_seen) < Duration::from_secs(30));
        out
    }
}

fn release(e: &mut Enemy, hash: u32, died: bool, out: &mut Tick) -> Role {
    e.epoch = e.epoch.saturating_add(1);
    e.owner = None;
    e.closer = None;
    e.unclaimed = None;
    out.frame.releases.push((hash, e.epoch, died));
    out.notices.push(Notice::Released { hash });
    Role::Local
}

/// The Wii U memory base and the codecave of `group` from Cemu's `log.txt` (latest boot).
pub fn parse_cemu_log(log: &str, group: &str) -> Option<(u64, u32)> {
    let hex_after = |line: &str, key: &str| -> Option<u64> {
        let rest = &line[line.find(key)? + key.len()..];
        let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        u64::from_str_radix(&rest[..end], 16).ok()
    };
    let base = log.lines().rev().find_map(|l| hex_after(l, "Init Wii U memory space (base: 0x"))?;
    let key = format!("{group}' (Codecave: ");
    let cave = log.lines().rev().find_map(|l| hex_after(l, &key))?;
    Some((base, u32::try_from(cave).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f32) -> Pose {
        Pose { pos: [x, 0.0, 0.0], rot: [0.0, 0.0, 0.0, 1.0], vel: [0.0; 3], anim: Anim::new("Wait") }
    }

    fn locals(list: &[(u32, f32)]) -> HashMap<u32, Local> {
        list.iter().map(|&(h, x)| (h, Local { pose: pose(x), life: 26 })).collect()
    }

    /// Two players' Syncs exchanging frames every tick (no latency).
    struct Pair {
        a: Sync,
        b: Sync,
        t: Instant,
        ms: u32,
    }

    impl Pair {
        fn new() -> Self {
            Pair { a: Sync::new("alice"), b: Sync::new("bob"), t: Instant::now(), ms: 0 }
        }
        /// One 16 ms step; returns (a's tick, b's tick).
        fn step(&mut self, la: [f32; 3], ea: &HashMap<u32, Local>, lb: [f32; 3], eb: &HashMap<u32, Local>) -> (Tick, Tick) {
            self.t += Duration::from_millis(16);
            self.ms += 16;
            let ta = self.a.tick(self.t, self.ms, Some(la), ea);
            let tb = self.b.tick(self.t, self.ms, Some(lb), eb);
            let fa = Frame::decode(&ta.frame.encode()).unwrap();
            let fb = Frame::decode(&tb.frame.encode()).unwrap();
            self.b.receive(self.t, self.ms as f64, fa);
            self.a.receive(self.t, self.ms as f64, fb);
            (ta, tb)
        }
        fn run(&mut self, n: usize, la: [f32; 3], ea: &HashMap<u32, Local>, lb: [f32; 3], eb: &HashMap<u32, Local>) -> (Tick, Tick) {
            let mut last = self.step(la, ea, lb, eb);
            for _ in 1..n {
                last = self.step(la, ea, lb, eb);
            }
            last
        }
    }

    #[test]
    fn frame_round_trip() {
        let f = Frame {
            from: "Kév".into(),
            t_ms: 123_456,
            link: Some([-1137.3, 257.9, 1776.5]),
            loaded: vec![0x636d844c, 0x52859ed1],
            owned: vec![Owned { hash: 0x636d844c, epoch: 3, pose: Pose { pos: [1.0, 2.0, 3.0], rot: normalize([0.0, 0.505, 0.0, 0.863]), vel: [0.1, 0.0, -0.2], anim: Anim::new("JumpAttackStartQuick") }, life: 26 }],
            handovers: vec![(7, 2, "bob".into())],
            releases: vec![(9, 4, true)],
        };
        let g = Frame::decode(&f.encode()).unwrap();
        assert_eq!(g.from, f.from);
        assert_eq!((g.t_ms, g.link, &g.loaded, &g.handovers, &g.releases), (f.t_ms, f.link, &f.loaded, &f.handovers, &f.releases));
        let (a, b) = (&f.owned[0], &g.owned[0]);
        assert_eq!((a.hash, a.epoch, a.pose.pos, a.pose.vel, a.life, a.pose.anim), (b.hash, b.epoch, b.pose.pos, b.pose.vel, b.life, b.pose.anim));
        assert_eq!(b.pose.anim.as_str(), "JumpAttackStartQuick");
        // A peer's AS name must be a plain, short name, or it is dropped (never written into the game).
        for bad in ["Run
X", "Dance%s", "Ä", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"] {
            let mut f2 = f.clone();
            f2.owned[0].pose.anim = Anim(*b"placeholder_overwritten!");
            let mut bytes = f2.encode();
            // Patch the encoded name in place: re-encode with the raw bad name.
            let good = f2.owned[0].pose.anim.as_str().to_owned();
            let at = bytes.windows(good.len()).position(|w| w == good.as_bytes()).unwrap();
            bytes.splice(at - 1..at + good.len(), std::iter::once(bad.len() as u8).chain(bad.bytes()));
            assert!(Frame::decode(&bytes).unwrap().owned[0].pose.anim.is_empty(), "{bad:?}");
        }
        assert_eq!(Anim::from_peer("JumpAttack_2").as_str(), "JumpAttack_2");
        // Over-long names are cut to fit the patch's buffer.
        assert_eq!(Anim::new("ABCDEFGHIJKLMNOPQRSTUVWXYZ").as_str(), "ABCDEFGHIJKLMNOPQRSTUVW");
        assert!(a.pose.rot.iter().zip(b.pose.rot).all(|(x, y)| (x - y).abs() < 1e-3));
        // Truncated or garbage input is refused, not a panic.
        let bytes = f.encode();
        for n in 0..bytes.len() {
            assert!(Frame::decode(&bytes[..n]).is_none());
        }
        assert!(Frame::decode(&[9, 9, 9]).is_none());
    }

    #[test]
    fn quaternion_matches_the_game() {
        // Measured live: a Bokoblin's actor matrix rows and its Havok rotation quaternion (sign-flipped, same rotation).
        let rows = [[0.49, 0.0, 0.872], [0.0, 1.0, 0.0], [-0.872, 0.0, 0.49]];
        let q = quat_from_rows(rows);
        assert!((q[1].abs() - 0.505).abs() < 0.01 && (q[3].abs() - 0.863).abs() < 0.01, "{q:?}");
        let back = rows_from_quat(q);
        for i in 0..3 {
            for j in 0..3 {
                assert!((back[i][j] - rows[i][j]).abs() < 0.01);
            }
        }
        // A tilted (ragdolling) rotation survives the round trip too.
        let q2 = normalize([0.3, -0.4, 0.2, 0.84]);
        let q3 = quat_from_rows(rows_from_quat(q2));
        let dot: f32 = (0..4).map(|i| q2[i] * q3[i]).sum();
        assert!(dot.abs() > 0.9999);
    }

    #[test]
    fn nearest_player_owns_and_the_other_gets_a_puppet() {
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        // alice 10 m away, bob 60 m away.
        let (ta, tb) = p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        assert_eq!(p.a.role(1), Role::Owner);
        assert_eq!(p.b.role(1), Role::Puppet);
        assert!(ta.puppets.is_empty());
        assert_eq!(tb.puppets, vec![1]);
        assert!(p.b.sample(1, p.ms as f64).is_some());
    }

    #[test]
    fn far_enemies_are_not_shared() {
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        // bob is outside the 150 m scope: both run their own.
        p.run(10, [10.0, 0.0, 0.0], &e, [400.0, 0.0, 0.0], &e);
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Local, Role::Local));
        // An enemy only one game has loaded is never shared.
        let (ta, tb) = p.run(10, [10.0, 0.0, 0.0], &locals(&[(2, 0.0)]), [20.0, 0.0, 0.0], &locals(&[]));
        assert_eq!(p.a.role(2), Role::Local);
        assert!(ta.puppets.is_empty() && tb.puppets.is_empty());
    }

    #[test]
    fn ownership_hands_over_with_hysteresis() {
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        assert_eq!(p.a.role(1), Role::Owner);
        // bob walks up: 5 m closer than alice is not enough.
        p.run(60, [10.0, 0.0, 0.0], &e, [5.0, 0.0, 0.0], &e);
        assert_eq!(p.a.role(1), Role::Owner);
        // alice walks away to 40 m: bob is 35 m closer, but only a short moment is not enough...
        p.run(10, [40.0, 0.0, 0.0], &e, [5.0, 0.0, 0.0], &e);
        assert_eq!(p.a.role(1), Role::Owner);
        // ...half a second is.
        let (ta, _) = p.run(40, [40.0, 0.0, 0.0], &e, [5.0, 0.0, 0.0], &e);
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Puppet, Role::Owner));
        assert_eq!(ta.puppets, vec![1]);
        // Never two owners; both frozen only while the hand-over message travels (one step here).
        let mut p = Pair::new();
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        let mut both_frozen = 0;
        for _ in 0..100 {
            p.step([40.0, 0.0, 0.0], &e, [5.0, 0.0, 0.0], &e);
            let (a, b) = (p.a.role(1), p.b.role(1));
            assert!(!(a == Role::Owner && b == Role::Owner), "two owners");
            both_frozen = if a == Role::Puppet && b == Role::Puppet { both_frozen + 1 } else { 0 };
            assert!(both_frozen <= 1, "both frozen for {both_frozen} steps");
        }
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Puppet, Role::Owner));
    }

    #[test]
    fn equal_claims_settle_on_one_owner() {
        // Both are exactly as far: both claim in the same tick; the smaller name keeps it.
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        p.run(5, [10.0, 0.0, 0.0], &e, [-10.0, 0.0, 0.0], &e);
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Owner, Role::Puppet));
    }

    #[test]
    fn disagreeing_copies_still_get_claimed() {
        // Each game's copy stands nearer the other player, so neither thinks it is nearest: the fallback claims.
        let mut p = Pair::new();
        let ea = locals(&[(1, 30.0)]);
        let eb = locals(&[(1, -30.0)]);
        p.run(10, [-10.0, 0.0, 0.0], &ea, [10.0, 0.0, 0.0], &eb);
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Local, Role::Local));
        p.run(150, [-10.0, 0.0, 0.0], &ea, [10.0, 0.0, 0.0], &eb);
        assert_eq!((p.a.role(1), p.b.role(1)), (Role::Owner, Role::Puppet));
    }

    #[test]
    fn released_when_the_enemy_dies_or_the_owner_goes_quiet() {
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        // Dies on the owner's side: released, bob's copy runs again (and dies through its own AI).
        let dead: HashMap<u32, Local> = [(1, Local { pose: pose(0.0), life: 0 })].into();
        let (ta, _) = p.run(3, [10.0, 0.0, 0.0], &dead, [60.0, 0.0, 0.0], &e);
        assert!(!ta.frame.releases.is_empty() || p.a.role(1) == Role::Local);
        assert_eq!(p.b.role(1), Role::Local);

        // Owner stops sending: after OWNER_STALE the puppet is freed.
        let mut p = Pair::new();
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        assert_eq!(p.b.role(1), Role::Puppet);
        let mut t = p.t;
        for i in 0..80 {
            t += Duration::from_millis(16);
            let tb = p.b.tick(t, p.ms + i * 16, Some([60.0, 0.0, 0.0]), &e);
            if i < 50 {
                assert_eq!(tb.puppets, vec![1], "still a puppet at {i}");
            }
        }
        assert_ne!(p.b.role(1), Role::Puppet);
    }

    #[test]
    fn stale_owner_epoch_is_ignored_after_release() {
        let now = Instant::now();
        let mut s = Sync::new("bob");
        s.tick(now, 0, Some([60.0, 0.0, 0.0]), &locals(&[(1, 0.0)]));
        let own = |epoch| Frame { from: "alice".into(), t_ms: 10, link: Some([0.0; 3]), loaded: vec![1], owned: vec![Owned { hash: 1, epoch, pose: pose(0.0), life: 5 }], ..Default::default() };
        s.receive(now, 10.0, own(1));
        s.receive(now, 11.0, Frame { from: "alice".into(), t_ms: 11, releases: vec![(1, 2, false)], ..Default::default() });
        // A delayed snapshot from before the release must not make it a puppet again.
        s.receive(now, 12.0, own(1));
        let t = s.tick(now, 12, Some([60.0, 0.0, 0.0]), &locals(&[(1, 0.0)]));
        assert!(t.puppets.is_empty());
    }

    #[test]
    fn puppets_play_the_owners_animation_in_time() {
        // The owner switches Run -> JumpAttack at sender time 160 ms; the puppet shows it one playback delay later,
        // together with the position of that moment (so the swing starts where it started for the owner).
        let mut tr = Track::default();
        for i in 0..40u32 {
            let mut p = pose(i as f32 * 0.1);
            p.anim = Anim::new(if i < 10 { "Run" } else { "JumpAttack" });
            tr.push(i as f64 * 16.0 + 3.0, i * 16, p);
        }
        let d = tr.delay_ms();
        let at = |t: f64| tr.sample(t).unwrap();
        assert_eq!(at(160.0 + 3.0 + d - 10.0).anim.as_str(), "Run");
        let s = at(160.0 + 3.0 + d + 1.0);
        assert_eq!(s.anim.as_str(), "JumpAttack");
        assert!((s.pos[0] - 1.0).abs() < 0.02, "{:?}", s.pos);
    }

    #[test]
    fn a_death_on_the_owner_kills_the_copies() {
        let mut p = Pair::new();
        let e = locals(&[(1, 0.0)]);
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        assert_eq!(p.b.owner_life(1), Some(26));
        // Damage on the owner shows in the puppet's mirrored life.
        let hurt: HashMap<u32, Local> = [(1, Local { pose: pose(0.0), life: 9 })].into();
        p.run(2, [10.0, 0.0, 0.0], &hurt, [60.0, 0.0, 0.0], &e);
        assert_eq!(p.b.owner_life(1), Some(9));
        // It dies on the owner: released with "died", and bob's copy is told to die.
        let dead: HashMap<u32, Local> = [(1, Local { pose: pose(0.0), life: 0 })].into();
        let (ta, _) = p.step([10.0, 0.0, 0.0], &dead, [60.0, 0.0, 0.0], &e);
        assert_eq!(ta.frame.releases, vec![(1, 2, true)]);
        let (_, tb) = p.step([10.0, 0.0, 0.0], &dead, [60.0, 0.0, 0.0], &e);
        assert_eq!(tb.kill, vec![1]);
        assert_eq!(p.b.role(1), Role::Local);
        // A plain release (nobody near any more) kills nothing.
        let mut p = Pair::new();
        p.run(5, [10.0, 0.0, 0.0], &e, [60.0, 0.0, 0.0], &e);
        let (_, tb) = p.run(5, [10.0, 0.0, 0.0], &e, [600.0, 0.0, 0.0], &e);
        assert!(tb.kill.is_empty());
    }

    #[test]
    fn spoofed_frames_are_ignored() {
        let now = Instant::now();
        let mut s = Sync::new("bob");
        s.tick(now, 0, Some([60.0, 0.0, 0.0]), &locals(&[(1, 0.0)]));
        let own = |from: &str, hash, epoch| Frame { from: from.into(), t_ms: 10, link: Some([0.0; 3]), loaded: vec![hash], owned: vec![Owned { hash, epoch, pose: pose(0.0), life: 5 }], ..Default::default() };
        s.receive(now, 10.0, own("alice", 1, 1));
        // An enemy we don't have loaded is not tracked at all.
        s.receive(now, 10.0, own("alice", 99, 1));
        assert!(s.track(99).is_none());
        // A third player can't hand over or release alice's enemy...
        s.receive(now, 11.0, Frame { from: "mallory".into(), handovers: vec![(1, 2, "mallory".into())], releases: vec![(1, 3, false)], ..Default::default() });
        // ...nor take it with a huge epoch.
        s.receive(now, 12.0, own("mallory", 1, u32::MAX));
        let t = s.tick(now + Duration::from_millis(16), 16, Some([60.0, 0.0, 0.0]), &locals(&[(1, 0.0)]));
        assert_eq!(t.puppets, vec![1]);
        assert_eq!(s.enemies[&1].owner.as_deref(), Some("alice"));
        assert_eq!(s.enemies[&1].epoch, 1);
    }

    #[test]
    fn puppet_playback_is_smooth_through_bunched_arrivals() {
        // Owner moves 0.1 m per 16 ms; every third frame arrives 17 ms late together with the next.
        let mut tr = Track::default();
        let mut shown = Vec::new();
        let mut arrivals: Vec<(f64, u32, f32)> = (0..200).map(|i| {
            let sent = i as u32 * 16;
            let late = if i % 3 == 1 { 17.0 } else { 0.0 };
            (sent as f64 + 5.0 + late, sent, i as f32 * 0.1)
        }).collect();
        arrivals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut k = 0;
        let mut t = 0.0;
        while t < 3000.0 {
            while k < arrivals.len() && arrivals[k].0 <= t {
                tr.push(arrivals[k].0, arrivals[k].1, pose(arrivals[k].2));
                k += 1;
            }
            if let Some(p) = tr.sample(t) {
                shown.push((t, p.pos[0]));
            }
            t += 4.0;
        }
        // In steady motion: every 4 ms step is 0.025 m (constant speed), and the lag behind the owner is ~delay.
        let steps: Vec<f32> = shown.windows(2).filter(|w| w[0].0 > 300.0 && w[1].0 < 3000.0).map(|w| w[1].1 - w[0].1).collect();
        let max = steps.iter().cloned().fold(0.0f32, f32::max);
        let min = steps.iter().cloned().fold(1.0f32, f32::min);
        let worst = shown.windows(2).filter(|w| w[0].0 > 300.0).max_by(|a, b| (a[1].1 - a[0].1).total_cmp(&(b[1].1 - b[0].1))).unwrap();
        assert!(max < 0.03 && min > 0.02, "steps {min}..{max} worst at {:?} delay {}", worst, tr.delay_ms());
        let (t1, x1) = shown.iter().find(|(t, _)| *t >= 2000.0).copied().unwrap();
        let owner_now = (t1 - 5.0) / 16.0 * 0.1;
        let lag_m = owner_now as f32 - x1;
        assert!((0.25..0.45).contains(&lag_m), "lag {lag_m} m");
    }

    #[test]
    fn delay_follows_the_jitter() {
        // Steady LAN arrivals (1 ms of jitter): the delay drops to the floor.
        let mut tr = Track::default();
        for i in 0..200u32 {
            tr.push(i as f64 * 16.0 + 3.0 + (i % 2) as f64, i * 16, pose(i as f32 * 0.1));
        }
        assert!((MIN_DELAY_MS..30.0).contains(&tr.delay_ms()), "{}", tr.delay_ms());
        // Every fourth snapshot 60 ms late (a relayed link): the delay grows past it.
        let mut tr = Track::default();
        for i in 0..200u32 {
            tr.push(i as f64 * 16.0 + 3.0 + if i % 4 == 0 { 60.0 } else { 0.0 }, i * 16, pose(i as f32 * 0.1));
        }
        assert!(tr.delay_ms() > 60.0, "{}", tr.delay_ms());
    }

    #[test]
    fn teleports_snap_and_restarted_senders_reset() {
        let mut tr = Track::default();
        tr.push(100.0, 0, pose(0.0));
        tr.push(116.0, 16, pose(50.0));
        let p = tr.sample(116.0 + INTERP_DELAY_MS - 8.0).unwrap();
        assert!(p.pos[0] == 0.0 || p.pos[0] == 50.0, "no gliding across a teleport: {:?}", p.pos);
        // A sender clock that jumps back (restarted sidecar) starts the buffer over.
        tr.push(200.0, 10_000, pose(1.0));
        tr.push(216.0, 10, pose(2.0));
        assert_eq!(tr.newest().unwrap().pos[0], 2.0);
    }

    #[test]
    fn log_parsing() {
        let log = "[22:14:13] Init Wii U memory space (base: 0x00000200C0000000)\n\
                   [22:14:13] Applying patch group 'BotW_PrivCordDowned_V208' (Codecave: 0180cc00-0180cd4c)\n\
                   [22:14:13] Applying patch group 'BotW_PrivCordEnemies_V208' (Codecave: 0180ce00-0180d09c)\n";
        assert_eq!(parse_cemu_log(log, PATCH_GROUP), Some((0x200C0000000, 0x0180ce00)));
        assert_eq!(parse_cemu_log(log, "Nope"), None);
    }

    #[tokio::test]
    async fn relay_forwards_frames_to_the_others_only() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(run_relay(l));
        let mut a = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut b = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let f = Frame { from: "alice".into(), t_ms: 1, ..Default::default() }.encode();
        write_frame(&mut a, &f).await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut b)).await.unwrap().unwrap().unwrap();
        assert_eq!(Frame::decode(&got).unwrap().from, "alice");
        // The sender doesn't get its own frame back.
        assert!(tokio::time::timeout(Duration::from_millis(150), read_frame(&mut a)).await.is_err());
        // An oversized frame drops that client, not the relay.
        use tokio::io::AsyncWriteExt;
        a.write_all(&(MAX_FRAME as u32 + 1).to_be_bytes()).await.unwrap();
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        write_frame(&mut c, &f).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), read_frame(&mut b)).await.unwrap().unwrap().is_some());
    }
}

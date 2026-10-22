//! Runs [`crate::botw_enemies`] on the live game: reads this game's enemies from Cemu's memory, swaps frames with
//! the other players through the enemy relay, freezes the enemies another player runs (the PrivCordEnemies patch's
//! puppet table) and writes their position and rotation in.
//!
//! What it touches (BotW v208, verified live 2026-10-20 on the main PC; see `docs/BOTWM-SHARED-ENEMIES-P0.md`):
//! - Link's actor: the PrivCordDowned block +0x20 (refreshed every frame); the actor list hangs off any actor
//!   (+0x4f0 → list, head node +0x18, node offset in the actor at list+0x24).
//! - Per enemy: matrix +0x1f8 (rows; translation +0x204/+0x214/+0x224), velocity +0x25c, ASList +0x394, ActorParam
//!   +0x39c (profile +0x54), physics +0x3a0, HashId +0x408, life +0x540.
//! - A puppet's position and rotation go into the actor matrix and its Havok motion state
//!   (`+0x3a0 → +0x50 → +4 → +0x80 → +0 → +0x5c → +0x18`): rotation columns +0x00, translation +0x30, centres of mass
//!   +0x40/+0x50 (x, y, z only; the w lanes hold Havok's timing), rotation quaternions +0x60/+0x70. Measured: with the
//!   AI skipped the enemy holds exactly what we write (2 cm median at 900 writes/s), and the rotation stays.
//!
//! One thread does it all on a 2 ms high-resolution timer: game ticks (read, rules, send) every 16 ms, puppet writes
//! every 4 ms. Writes use `NtWriteVirtualMemory` (see `botw_smooth_sidecar`). Nothing is injected. Made by Blackz.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::mpsc;

use crate::botw_downed_sidecar::mem::Cemu;
use crate::botw_enemies::{self as rules, Anim, Frame, Local, Notice, Pose, Sync};

/// What the sidecar reports.
#[derive(Clone, Debug)]
pub enum Event {
    /// Found Cemu with both patches loaded (guest address of the puppet table block).
    Attached { cave: u32 },
    Notice(Notice),
    /// Every two seconds.
    Stats(Stats),
}

/// Diagnostics for the last two seconds.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Our Link's position (None before a save is loaded).
    pub link: Option<[f32; 3]>,
    /// Live map enemies loaded here.
    pub enemies: usize,
    pub owned: usize,
    pub puppets: usize,
    pub peers: usize,
    pub frames_in: u32,
    pub frames_out: u32,
    /// AI updates the patch skipped (puppets frozen in game).
    pub skips: u32,
    /// How far our puppets were from the owner's enemy at the same moment (metres): median, 95th percentile, max.
    pub sync_err_p50: f32,
    pub sync_err_p95: f32,
    pub sync_err_max: f32,
    /// Largest jump of any puppet between two game ticks (metres; a snap would show here).
    pub max_step: f32,
    /// AS changes asked of puppets, and played by the patch.
    pub anim_requests: u32,
    pub anim_plays: u32,
    /// How long after the owner's enemy switched AS our puppet switched to the same one (ms): median, max.
    pub anim_lag_p50: f32,
    pub anim_lag_max: f32,
    /// Shared enemies right now: HashId, our role, and where our copy stands.
    pub shared: Vec<SharedEnemy>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedEnemy {
    pub hash: u32,
    pub owner: bool,
    pub pos: [f32; 3],
    /// Its current AS in our game.
    pub anim: String,
}

/// A running sidecar; [`EnemySidecar::stop`] ends it and frees every puppet.
pub struct EnemySidecar {
    stop: Arc<AtomicBool>,
    relay_stop: tokio::sync::watch::Sender<bool>,
}

impl EnemySidecar {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.relay_stop.send(true);
    }
}

/// Start for player `me` with Cemu at `cemu_root` (folder with `Cemu.exe` and `log.txt`), talking to the enemy relay
/// at `relay`. Must be called inside a Tokio runtime.
pub fn start(cemu_root: PathBuf, me: String, relay: std::net::SocketAddr, events: mpsc::UnboundedSender<Event>) -> EnemySidecar {
    let stop = Arc::new(AtomicBool::new(false));
    let (relay_stop, relay_stop_rx) = tokio::sync::watch::channel(false);
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(relay_client(relay, in_tx, out_rx, relay_stop_rx));
    let s = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("botw-enemies".into())
        .spawn(move || run(&cemu_root, me, in_rx, out_tx, events, &s))
        .ok();
    EnemySidecar { stop, relay_stop }
}

async fn relay_client(relay: std::net::SocketAddr, in_tx: mpsc::UnboundedSender<Vec<u8>>, mut out_rx: mpsc::UnboundedReceiver<Vec<u8>>, mut stop: tokio::sync::watch::Receiver<bool>) {
    while !*stop.borrow() {
        let sock = match tokio::net::TcpStream::connect(relay).await {
            Ok(s) => s,
            Err(_) => {
                while out_rx.try_recv().is_ok() {}
                tokio::select! { _ = tokio::time::sleep(Duration::from_secs(2)) => continue, _ = stop.changed() => return }
            }
        };
        let _ = sock.set_nodelay(true);
        let (mut r, mut w) = sock.into_split();
        // Reads in their own task so a frame is never cut in half by the select below.
        let in_tx2 = in_tx.clone();
        let mut reader = tokio::spawn(async move {
            while let Ok(Some(f)) = rules::read_frame(&mut r).await {
                if in_tx2.send(f).is_err() {
                    break;
                }
            }
        });
        loop {
            tokio::select! {
                _ = &mut reader => break,
                f = out_rx.recv() => match f {
                    Some(f) => if rules::write_frame(&mut w, &f).await.is_err() { break },
                    None => { reader.abort(); return }
                },
                _ = stop.changed() => { reader.abort(); return }
            }
        }
        reader.abort();
    }
}

/// An enemy actor found in the list.
#[derive(Clone, Copy, Debug)]
struct Known {
    actor: u32,
    /// Slot 0's AS player (its current AS name is a `char*` at +4).
    as_player: Option<u32>,
}

/// A puppet we drive: its Havok motion state and centre-of-mass offset, and what we last wrote.
#[derive(Clone, Copy, Debug)]
struct Puppet {
    actor: u32,
    motion: Option<u32>,
    com_off: [f32; 3],
    last: Option<(Pose, Instant)>,
    last_pos: Option<[f32; 3]>,
    /// Where our own copy stood when it became a puppet, and when (it glides to the owner's from there).
    start: Option<(Pose, Instant)>,
    /// Its entry in the patch's table, the last AS request number and AS asked for.
    slot: usize,
    req: u32,
    anim: Anim,
    /// The AS our copy showed at the last game tick.
    seen_anim: Anim,
}

const ACTOR_READ: u32 = 0x1f8;
const ACTOR_READ_LEN: usize = 0x34c; // through life +0x540
const OFF_HASH: usize = 0x408 - 0x1f8;
const OFF_VEL: usize = 0x25c - 0x1f8;
const OFF_LIFE: usize = 0x540 - 0x1f8;

fn be_f32(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32::from_be_bytes(b[o..o + 4].try_into().unwrap()))
}

fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

/// Pose and life from the bytes at actor+0x1f8 (None if it isn't `hash` any more or holds garbage).
fn parse_actor(b: &[u8], hash: u32) -> Option<Local> {
    if b.len() < ACTOR_READ_LEN || be_u32(b, OFF_HASH) != hash {
        return None;
    }
    let m: Vec<f32> = (0..12).map(|i| be_f32(b, i * 4)).collect();
    if m.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let rows = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
    let vel = [be_f32(b, OFF_VEL), be_f32(b, OFF_VEL + 4), be_f32(b, OFF_VEL + 8)];
    let pose = Pose { pos: [m[3], m[7], m[11]], rot: rules::quat_from_rows(rows), vel: vel.map(|v| if v.is_finite() { v } else { 0.0 }), anim: Anim::default() };
    Some(Local { pose, life: be_u32(b, OFF_LIFE) as i32 })
}

struct Game {
    c: Cemu,
    downed: u32,
    cave: u32,
}

fn attach(cemu_root: &Path) -> Option<Game> {
    let log = std::fs::read_to_string(cemu_root.join("log.txt")).ok()?;
    let (base, cave) = rules::parse_cemu_log(&log, rules::PATCH_GROUP)?;
    let (_, downed) = crate::botw_downed::parse_cemu_log(&log)?;
    let c = Cemu::open(&cemu_root.join("Cemu.exe"), base)?;
    (c.read_u32(cave)? == rules::MAGIC && c.read_u32(downed)? == crate::botw_downed::MAGIC).then_some(())?;
    tracing::info!(cave = format_args!("{cave:#x}"), "enemies: attached to Cemu");
    Some(Game { c, downed, cave })
}

impl Game {
    fn cstr(&self, a: u32, n: usize) -> Option<String> {
        let b = self.c.read(a, n)?;
        let end = b.iter().position(|&c| c == 0)?;
        String::from_utf8(b[..end].to_vec()).ok()
    }

    /// Walk the actor list: live, map-placed enemies by HashId. `profiles` caches each actor's enemy check.
    fn scan(&self, link: u32, profiles: &mut HashMap<(u32, u32), bool>) -> HashMap<u32, Known> {
        let mut out: HashMap<u32, Known> = HashMap::new();
        let c = &self.c;
        let Some(list) = c.read_u32(link + 0x4f0) else { return out };
        let (Some(off), head) = (c.read_u32(list + 0x24), list + 0x18) else { return out };
        let mut n = c.read_u32(head).unwrap_or(0);
        let mut seen = 0;
        let mut alive = std::collections::HashSet::new();
        while n != 0 && n != head && seen < 5000 {
            seen += 1;
            let actor = n.wrapping_sub(off);
            n = c.read_u32(n).unwrap_or(0);
            let Some(hash) = c.read_u32(actor + 0x408).filter(|&h| h != 0) else { continue };
            alive.insert((actor, hash));
            let enemy = *profiles.entry((actor, hash)).or_insert_with(|| {
                let prof = c.read_u32(actor + 0x39c).filter(|&p| p != 0).and_then(|p| c.read_u32(p + 0x54)).and_then(|s| self.cstr(s, 24));
                prof.is_some_and(|p| rules::ENEMY_PROFILES.contains(&p.as_str()))
            });
            if !enemy {
                continue;
            }
            // A killed enemy stays as a placeholder (same HashId, life 1, no AS) until the Blood Moon: skip it.
            // (The placeholder's AS name pointer can be set but empty, so look at the name itself.)
            let as_player = self.as_player(actor);
            let life = c.read_u32(actor + 0x540).map(|v| v as i32).unwrap_or(0);
            if life <= 1 && as_player.and_then(|p| self.anim(p)).is_none_or(|a| a.is_empty()) {
                continue;
            }
            out.insert(hash, Known { actor, as_player });
        }
        profiles.retain(|k, _| alive.contains(k));
        out
    }

    /// Slot 0's AS player: ASList (+0x394) → slots (+0x98, 0x34 bytes each) → layers (+0x1c) → [0]. Verified live:
    /// its +4 is the current AS name (`Talk`, `Squat`, ...); slot 1 holds the other body part.
    fn as_player(&self, actor: u32) -> Option<u32> {
        let c = &self.c;
        let asl = c.read_u32(actor + 0x394).filter(|&a| a != 0)?;
        let slots = c.read_u32(asl + 0x98).filter(|&a| a != 0)?;
        let layers = c.read_u32(slots + 0x1c).filter(|&a| a != 0)?;
        c.read_u32(layers).filter(|&a| a != 0)
    }

    fn anim(&self, as_player: u32) -> Option<Anim> {
        let name = self.c.read_u32(as_player + 4).filter(|&a| a != 0)?;
        let b = self.c.read(name, 24)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(rules::Anim::MAX);
        Some(Anim::new(std::str::from_utf8(&b[..end.min(Anim::MAX)]).ok()?))
    }

    fn entry(&self, slot: usize) -> u32 {
        self.cave + rules::OFF_TABLE + slot as u32 * rules::ENTRY_LEN
    }

    /// Put `hash` in table entry `slot` with a fresh lease and no pending AS request.
    fn init_entry(&self, slot: usize, hash: u32) {
        let e = self.entry(slot);
        let mut b = Vec::with_capacity(rules::ENTRY_LEN as usize);
        for v in [hash, rules::LEASE, 0, 0, e + rules::ENTRY_NAME, rules::SAFESTRING_VTABLE] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&[0u8; 24]);
        self.c.write(e, &b);
    }

    /// Ask the patch to play `anim` on the puppet in `slot` (name first, then the request number).
    fn request_anim(&self, slot: usize, req: u32, anim: Anim) {
        let e = self.entry(slot);
        self.c.write(e + rules::ENTRY_NAME, &anim.0);
        self.c.write_u32(e + rules::ENTRY_REQ, req);
    }

    fn motion_state(&self, actor: u32) -> Option<u32> {
        let mut p = actor;
        for o in [0x3a0, 0x50, 4, 0x80, 0, 0x5c, 0x18] {
            p = self.c.read_u32(p + o).filter(|&v| v != 0)?;
        }
        Some(p)
    }

    fn v3(&self, a: u32) -> Option<[f32; 3]> {
        let b = self.c.read(a, 12)?;
        Some([be_f32(&b, 0), be_f32(&b, 4), be_f32(&b, 8)])
    }

    /// Refresh the table from `slots` (HashId per entry, 0 = free): every used entry's HashId and lease, free ones
    /// zeroed, and the count up to the last used entry. AS request fields are left alone.
    fn write_table(&self, slots: &[u32]) {
        for (i, &h) in slots.iter().enumerate().take(rules::TABLE_LEN) {
            let mut b = [0u8; 8];
            b[..4].copy_from_slice(&h.to_be_bytes());
            if h != 0 {
                b[4..].copy_from_slice(&rules::LEASE.to_be_bytes());
            }
            self.c.write(self.entry(i), &b);
        }
        let n = slots.iter().rposition(|&h| h != 0).map_or(0, |i| i + 1).min(rules::TABLE_LEN);
        self.c.write_u32(self.cave + rules::OFF_COUNT, n as u32);
    }

    fn write_pose(&self, p: &Puppet, pose: &Pose) {
        let r = rules::rows_from_quat(pose.rot);
        let [x, y, z] = pose.pos;
        let mut mtx = Vec::with_capacity(48);
        for (row, t) in r.iter().zip([x, y, z]) {
            for v in [row[0], row[1], row[2], t] {
                mtx.extend_from_slice(&v.to_be_bytes());
            }
        }
        self.c.write(p.actor + 0x1f8, &mtx);
        let Some(ms) = p.motion else { return };
        let mut cols = Vec::with_capacity(48);
        for j in 0..3 {
            for v in [r[0][j], r[1][j], r[2][j], 0.0] {
                cols.extend_from_slice(&f32::to_be_bytes(v));
            }
        }
        self.c.write(ms, &cols);
        let v3 = |v: [f32; 3]| v.iter().flat_map(|f| f.to_be_bytes()).collect::<Vec<u8>>();
        let com = [x + p.com_off[0], y + p.com_off[1], z + p.com_off[2]];
        self.c.write(ms + 0x30, &v3(pose.pos));
        self.c.write(ms + 0x40, &v3(com));
        self.c.write(ms + 0x50, &v3(com));
        let q: Vec<u8> = pose.rot.iter().flat_map(|f| f.to_be_bytes()).collect();
        self.c.write(ms + 0x60, &[q.as_slice(), q.as_slice()].concat());
    }
}

fn percentile(v: &mut [f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * p).round() as usize]
}

fn run(cemu_root: &Path, me: String, mut in_rx: mpsc::UnboundedReceiver<Vec<u8>>, out_tx: mpsc::UnboundedSender<Vec<u8>>, events: mpsc::UnboundedSender<Event>, stop: &AtomicBool) {
    let waiter = timer::Waiter::new();
    let start = Instant::now();
    let local_ms = |t: Instant| t.duration_since(start).as_secs_f64() * 1000.0;
    let mut sync = Sync::new(me);
    let mut game: Option<Game> = None;
    let mut known: HashMap<u32, Known> = HashMap::new();
    let mut profiles: HashMap<(u32, u32), bool> = HashMap::new();
    let mut puppets: HashMap<u32, Puppet> = HashMap::new();
    // HashId per puppet table entry (0 = free); an entry keeps its puppet while it stays one.
    let mut slots: Vec<u32> = Vec::new();
    let mut lags: Vec<f32> = Vec::new();
    let mut plays0: Option<u32> = None;
    let far = Instant::now() - Duration::from_secs(60);
    let (mut last_attach, mut last_scan, mut last_tick, mut last_table, mut last_write, mut last_stats) = (far, far, far, far, far, Instant::now());
    let mut stats = Stats::default();
    let mut errs: Vec<f32> = Vec::new();
    // (sender time, our puppet's position then) per puppet, checked once the owner's snapshots for that time arrive.
    let mut pending: HashMap<u32, VecDeque<(f64, [f32; 3])>> = HashMap::new();
    let mut skips0: Option<u32> = None;
    let mut locals: HashMap<u32, Local> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        while let Ok(b) = in_rx.try_recv() {
            if let Some(f) = Frame::decode(&b) {
                stats.frames_in += 1;
                sync.receive(now, local_ms(now), f);
            }
        }
        if game.as_ref().is_some_and(|g| !g.c.alive()) {
            game = None;
            known.clear();
            puppets.clear();
            slots.clear();
        }
        if game.is_none() && now.duration_since(last_attach) >= Duration::from_secs(1) {
            last_attach = now;
            game = attach(cemu_root);
            if let Some(g) = &game {
                g.write_table(&[0; rules::TABLE_LEN]);
                let _ = events.send(Event::Attached { cave: g.cave });
            }
        }
        let Some(g) = &game else {
            waiter.wait(50_000);
            continue;
        };

        if now.duration_since(last_tick) >= Duration::from_millis(16) {
            last_tick = now;
            let link_actor = g.c.read_u32(g.downed + 0x20).unwrap_or(0);
            let link = (link_actor != 0)
                .then(|| {
                    let p = crate::botw_downed::ACTOR_POS.map(|o| g.c.read_u32(link_actor + o).map(f32::from_bits));
                    Some([p[0]?, p[1]?, p[2]?])
                })
                .flatten();
            if link_actor != 0 && now.duration_since(last_scan) >= Duration::from_millis(500) {
                last_scan = now;
                known = g.scan(link_actor, &mut profiles);
            }
            locals.clear();
            for (&hash, k) in &known {
                if let Some(mut l) = g.c.read(k.actor + ACTOR_READ, ACTOR_READ_LEN).and_then(|b| parse_actor(&b, hash)) {
                    l.pose.anim = k.as_player.and_then(|p| g.anim(p)).unwrap_or_default();
                    locals.insert(hash, l);
                }
            }
            let tick = sync.tick(now, local_ms(now) as u32, link, &locals);
            if out_tx.send(tick.frame.encode()).is_ok() {
                stats.frames_out += 1;
            }
            for n in tick.notices {
                tracing::debug!(?n, "enemies");
                let _ = events.send(Event::Notice(n));
            }
            // Died in the owner's game: our copy (freed this tick) dies too, through its own AI.
            for h in &tick.kill {
                if let Some(k) = known.get(h) {
                    g.c.write_u32(k.actor + 0x540, 0);
                }
            }
            // Puppets: new ones get a table entry and their motion state looked up; gone ones free their entry.
            puppets.retain(|h, p| {
                let keep = tick.puppets.contains(h) && known.get(h).is_some_and(|k| k.actor == p.actor);
                if !keep {
                    slots[p.slot] = 0;
                }
                keep
            });
            let mut changed = false;
            for &h in &tick.puppets {
                let Some(k) = known.get(&h) else { continue };
                if !puppets.contains_key(&h) {
                    let Some(slot) = slots.iter().position(|&s| s == 0).or((slots.len() < rules::TABLE_LEN).then_some(slots.len())) else {
                        continue; // table full: this one keeps its own AI
                    };
                    if slot == slots.len() {
                        slots.push(0);
                    }
                    slots[slot] = h;
                    g.init_entry(slot, h);
                    changed = true;
                    let motion = g.motion_state(k.actor);
                    let com_off = motion
                        .and_then(|ms| Some((g.v3(ms + 0x30)?, g.v3(ms + 0x40)?)))
                        .map(|(t, c)| [c[0] - t[0], c[1] - t[1], c[2] - t[2]])
                        .filter(|o| o.iter().all(|v| v.is_finite() && v.abs() < 5.0))
                        .unwrap_or([0.0; 3]);
                    let start = locals.get(&h).map(|l| (l.pose, now));
                    puppets.insert(h, Puppet { actor: k.actor, motion, com_off, last: None, last_pos: None, start, slot, req: 0, anim: Anim::default(), seen_anim: Anim::default() });
                }
                let p = puppets.get_mut(&h).unwrap();
                // Damage done in the owner's game shows here too (never raised, never to 0: death comes as a kill).
                if let (Some(ol), Some(l)) = (sync.owner_life(h), locals.get(&h)) {
                    if ol > 0 && ol < l.life {
                        g.c.write_u32(k.actor + 0x540, ol as u32);
                    }
                }
                // Measure the animation: when our copy switches AS, how long after the owner's switched to it.
                if let (Some(l), Some(tr)) = (locals.get(&h), sync.track(h)) {
                    if l.pose.anim != p.seen_anim && !l.pose.anim.is_empty() {
                        if let Some(s) = tr.sender_time(local_ms(now)) {
                            if let Some(t) = tr.anim_started(s, l.pose.anim) {
                                lags.push((s - t) as f32);
                            }
                        }
                    }
                    p.seen_anim = l.pose.anim;
                }
                // Measure: our copy's position now vs the owner's at the same moment (checked when that moment's
                // snapshots are in).
                if let (Some(l), Some(tr)) = (locals.get(&h), sync.track(h)) {
                    if let Some(prev) = p.last_pos {
                        stats.max_step = stats.max_step.max(rules::dist(prev, l.pose.pos));
                    }
                    p.last_pos = Some(l.pose.pos);
                    if let Some(s) = tr.sender_time(local_ms(now)) {
                        pending.entry(h).or_default().push_back((s, l.pose.pos));
                    }
                }
            }
            pending.retain(|h, q| {
                let Some(tr) = sync.track(*h).filter(|_| puppets.contains_key(h)) else { return false };
                while let Some(&(s, pos)) = q.front() {
                    if !tr.covers(s) {
                        if q.len() > 200 {
                            q.pop_front();
                            continue;
                        }
                        break;
                    }
                    if let Some(o) = tr.at(s) {
                        errs.push(rules::dist(o.pos, pos));
                    }
                    q.pop_front();
                }
                true
            });
            if changed || now.duration_since(last_table) >= rules::TABLE_REFRESH {
                while slots.last() == Some(&0) {
                    slots.pop();
                }
                g.write_table(&slots);
                last_table = now;
            }
            stats.link = link;
            stats.shared = locals
                .iter()
                .filter_map(|(&hash, l)| match sync.role(hash) {
                    rules::Role::Owner => Some(SharedEnemy { hash, owner: true, pos: l.pose.pos, anim: l.pose.anim.as_str().to_owned() }),
                    rules::Role::Puppet => Some(SharedEnemy { hash, owner: false, pos: l.pose.pos, anim: l.pose.anim.as_str().to_owned() }),
                    rules::Role::Local => None,
                })
                .collect();
            stats.shared.sort_by_key(|e| e.hash);
            stats.enemies = locals.len();
            stats.puppets = puppets.len();
            stats.owned = tick.frame.owned.len();
        }

        if now.duration_since(last_write) >= Duration::from_millis(4) {
            last_write = now;
            let t = local_ms(now);
            for (&h, p) in puppets.iter_mut() {
                let Some(mut pose) = sync.sample(h, t) else { continue };
                if let Some((from, at)) = p.start {
                    let k = now.duration_since(at).as_secs_f64() * 1000.0 / rules::BLEND_IN_MS;
                    if k >= 1.0 {
                        p.start = None;
                    } else {
                        pose = rules::blend(&from, &pose, k as f32);
                    }
                }
                // Unchanged poses are rewritten every 50 ms anyway (physics may have nudged the copy).
                let fresh = p.last.is_none_or(|(lp, at)| lp != pose || now.duration_since(at) >= Duration::from_millis(50));
                if fresh {
                    g.write_pose(p, &pose);
                    p.last = Some((pose, now));
                }
                // The owner's AS at this playback moment: ask the patch to play it when it changes.
                if !pose.anim.is_empty() && pose.anim != p.anim {
                    p.req = p.req.wrapping_add(1);
                    g.request_anim(p.slot, p.req, pose.anim);
                    p.anim = pose.anim;
                    stats.anim_requests += 1;
                }
            }
        }

        if now.duration_since(last_stats) >= Duration::from_secs(2) {
            last_stats = now;
            let skips = g.c.read_u32(g.cave + rules::OFF_SKIPS).unwrap_or(0);
            stats.skips = skips0.map_or(0, |s| skips.wrapping_sub(s));
            skips0 = Some(skips);
            stats.peers = sync.peers();
            stats.sync_err_p50 = percentile(&mut errs, 0.5);
            stats.sync_err_p95 = percentile(&mut errs, 0.95);
            stats.sync_err_max = errs.iter().copied().fold(0.0, f32::max);
            errs.clear();
            let plays = g.c.read_u32(g.cave + rules::OFF_PLAYS).unwrap_or(0);
            stats.anim_plays = plays0.map_or(0, |p| plays.wrapping_sub(p));
            plays0 = Some(plays);
            stats.anim_lag_p50 = percentile(&mut lags, 0.5);
            stats.anim_lag_max = lags.iter().copied().fold(0.0, f32::max);
            lags.clear();
            let _ = events.send(Event::Stats(stats.clone()));
            stats = Stats::default();
        }
        waiter.wait(if puppets.is_empty() { 4_000 } else { 2_000 });
    }
    // Stopped: every copy runs on its own again at once.
    if let Some(g) = &game {
        g.write_table(&[0; rules::TABLE_LEN]);
    }
}

mod timer {
    /// Sleeps on a high-resolution waitable timer (Windows' plain sleep rounds up to ~1-15 ms).
    pub struct Waiter {
        #[cfg(windows)]
        h: windows_sys::Win32::Foundation::HANDLE,
    }

    // SAFETY: a timer handle is usable from any thread; this one is only used by the thread that owns the Waiter.
    unsafe impl Send for Waiter {}

    impl Waiter {
        #[cfg(windows)]
        pub fn new() -> Self {
            use windows_sys::Win32::System::Threading::{CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, TIMER_ALL_ACCESS};
            Waiter { h: unsafe { CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) } }
        }
        #[cfg(not(windows))]
        pub fn new() -> Self {
            Waiter {}
        }

        pub fn wait(&self, micros: i64) {
            #[cfg(windows)]
            unsafe {
                use windows_sys::Win32::System::Threading::{INFINITE, SetWaitableTimer, WaitForSingleObject};
                if self.h.is_null() {
                    std::thread::sleep(std::time::Duration::from_micros(micros as u64));
                } else {
                    let due = -(micros * 10);
                    SetWaitableTimer(self.h, &due, 0, None, std::ptr::null(), 0);
                    WaitForSingleObject(self.h, INFINITE);
                }
            }
            #[cfg(not(windows))]
            std::thread::sleep(std::time::Duration::from_micros(micros as u64));
        }
    }

    #[cfg(windows)]
    impl Drop for Waiter {
        fn drop(&mut self) {
            if !self.h.is_null() {
                unsafe { windows_sys::Win32::Foundation::CloseHandle(self.h) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_bytes_parse() {
        let mut b = vec![0u8; ACTOR_READ_LEN];
        let rows = [[0.49f32, 0.0, 0.872, -1152.49], [0.0, 1.0, 0.0, 255.73], [-0.872, 0.0, 0.49, 1769.02]];
        for (i, v) in rows.iter().flatten().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        b[OFF_HASH..OFF_HASH + 4].copy_from_slice(&0x636d844cu32.to_be_bytes());
        b[OFF_VEL..OFF_VEL + 4].copy_from_slice(&0.1f32.to_be_bytes());
        b[OFF_LIFE..OFF_LIFE + 4].copy_from_slice(&26i32.to_be_bytes());
        let l = parse_actor(&b, 0x636d844c).unwrap();
        assert_eq!(l.pose.pos, [-1152.49, 255.73, 1769.02]);
        assert_eq!((l.life, l.pose.vel[0]), (26, 0.1));
        assert!((l.pose.rot[1].abs() - 0.505).abs() < 0.01);
        // A recycled actor (other HashId) is not trusted.
        assert!(parse_actor(&b, 1).is_none());
    }
}

//! BotW co-op "downed / revive": the rules PrivCord runs next to the game, and the tiny relay that lets the players'
//! PrivCords see each other.
//!
//! The game side is a Cemu patch (`src-tauri/resources/botwmp-downed/patch_PrivCordDowned.asm`): when armed (mode 1) a
//! hit that would kill Link leaves him at 1/4 heart and raises `DownState`; mode 2 lets him die. Its codecave block is
//! read and written from outside Cemu (`src-tauri/src/botw_downed.rs`); this module decides what to write.
//!
//! Rules (Blackz, 2026-10-19):
//! - A downed player stays down at 1/4 heart (food is undone) until a friend revives them. There is no bleed-out timer.
//! - A friend who is up and within [`REVIVE_RADIUS`] sees a prompt in PrivCord's overlay and holds the revive key for
//!   [`REVIVE_HOLD`]; moving away or letting go cancels. Both players see the progress.
//! - Only when nobody is left standing (every player in the session is down, or a player is down alone) does everyone
//!   die the vanilla way and reload ([`ALL_DOWN_GRACE`] after it happens, so a revive finishing at that moment wins).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How close (game units = metres) a friend must stand to revive.
pub const REVIVE_RADIUS: f32 = 3.0;
/// How long the friend holds the revive key.
pub const REVIVE_HOLD: Duration = Duration::from_secs(12);
/// A started revive keeps going while the friend stays within this distance.
pub const REVIVE_KEEP_RADIUS: f32 = 4.5;
/// Key-up or out-of-reach blips shorter than this don't reset a revive.
pub const REVIVE_GRACE: Duration = Duration::from_millis(600);
/// Health after a revive, in quarter hearts (3 hearts).
pub const REVIVE_LIFE: i32 = 12;
/// How long "nobody is standing" must last before everyone dies.
pub const ALL_DOWN_GRACE: Duration = Duration::from_secs(3);
/// A friend's messages older than this are ignored (they left or their PrivCord stopped).
pub const PEER_STALE: Duration = Duration::from_secs(3);
/// A friend who went silent (PrivCord restart, network blip) still counts as possibly standing for this long, so a
/// downed player isn't killed because their helper's app hiccupped (that happened in the first two-PC test).
pub const FRIEND_SILENT: Duration = Duration::from_secs(30);
/// Tunnelled TCP port of the relay on the host (5051 is the co-op party feed of the `botwmp-coop` branch).
pub const RELAY_PORT: u16 = 5053;
/// Windows virtual-key code of the revive key: G (unused by PrivCord's keyboard profile).
pub const REVIVE_KEY_VK: i32 = 0x47;

/// Magic of the codecave block, layout version 1 ("PCD1").
pub const MAGIC: u32 = 0x5043_4431;
/// Bytes of the block PrivCord reads.
pub const BLOCK_LEN: usize = 40;
/// Byte offsets of the writable fields in the block.
pub const OFF_MODE: u32 = 4;
pub const OFF_STATE: u32 = 8;
/// The input-freeze lease (controller reads the patch may still empty); refilled every tick to [`LEASE`].
pub const OFF_LEASE: u32 = 44;
/// ~1 s of controller reads at 60 fps: if PrivCord stops, a downed player gets their controls back that fast.
pub const LEASE: u32 = 60;
/// Link's world position in his actor (big-endian floats).
pub const ACTOR_POS: [u32; 3] = [0x204, 0x214, 0x224];

/// The patch's codecave block, as read from guest memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub mode: u32,
    pub state: u32,
    pub count: u32,
    pub hits: u32,
    /// Guest address of Link's health (from the last hit; 0 before the first).
    pub life_ptr: u32,
    pub last_actor: u32,
    pub last_life: i32,
    /// Link's actor, refreshed every frame.
    pub self_actor: u32,
    pub frames: u32,
}

impl Block {
    pub fn parse(b: &[u8]) -> Option<Block> {
        if b.len() < BLOCK_LEN {
            return None;
        }
        let w = |i: usize| u32::from_be_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        (w(0) == MAGIC).then(|| Block {
            mode: w(1),
            state: w(2),
            count: w(3),
            hits: w(4),
            life_ptr: w(5),
            last_actor: w(6),
            last_life: w(7) as i32,
            self_actor: w(8),
            frames: w(9),
        })
    }
}

/// From Cemu's `log.txt` (latest boot): the host address of guest memory and the guest address of the patch's
/// codecave block. `None` until the game has booted with the patch.
pub fn parse_cemu_log(log: &str) -> Option<(u64, u32)> {
    let hex_after = |line: &str, key: &str| -> Option<u64> {
        let rest = &line[line.find(key)? + key.len()..];
        let end = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        u64::from_str_radix(&rest[..end], 16).ok()
    };
    let base = log.lines().rev().find_map(|l| hex_after(l, "Init Wii U memory space (base: 0x"))?;
    let cave = log.lines().rev().find_map(|l| hex_after(l, "BotW_PrivCordDowned_V208' (Codecave: "))?;
    Some((base, u32::try_from(cave).ok()?))
}

/// One JSON line on the relay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum Msg {
    /// Sent a few times a second by every player.
    #[serde(rename_all = "camelCase")]
    Status { name: String, downed: bool, pos: Option<[f32; 3]> },
    /// `by` is holding the revive key next to `target`; `percent` 0–100.
    #[serde(rename_all = "camelCase")]
    Reviving { by: String, target: String, percent: u8 },
    /// `by` finished reviving `target`.
    #[serde(rename_all = "camelCase")]
    Revive { by: String, target: String },
}

/// Something to write into the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    Mode(u32),
    State(u32),
    Life(i32),
}

/// Something to tell the player (a toast in PrivCord).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Notice {
    /// This player is down.
    Downed,
    /// A friend revived this player.
    Revived { by: String },
    /// This player revived a friend.
    RevivedFriend { name: String },
    /// Nobody is left standing: game over for everyone.
    AllDown,
    /// A friend is down.
    FriendDowned { name: String },
    /// A friend is back up.
    FriendUp { name: String },
}

/// What PrivCord's overlay over the game shows. Serialized for the frontend.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overlay {
    /// This player is down.
    pub downed: bool,
    /// A friend is reviving this player: (their name, percent).
    pub revived_by: Option<(String, u8)>,
    /// A downed friend within reach: their name, and the revive progress if the key is held.
    pub prompt: Option<(String, u8)>,
    /// Downed friends (for the "go help" line).
    pub friends_down: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Up,
    Downed { alone_since: Option<Instant> },
    Dying,
}

/// The per-player rules. Feed it a fresh [`Block`] (plus health, position and key state) about ten times a second, and
/// every relay message with [`Downed::receive`].
pub struct Downed {
    me: String,
    phase: Phase,
    actor: u32,
    peers: HashMap<String, (Instant, bool, Option<[f32; 3]>)>,
    /// Last downed flag seen per friend, for the FriendDowned / FriendUp notices.
    was_down: HashMap<String, bool>,
    /// Someone reviving me: (by, percent, when heard).
    revived_by: Option<(String, u8, Instant)>,
    /// A revive that arrived for me.
    revive_cmd: Option<String>,
    /// My hold on a downed friend: (target, started, last tick the key was down with them in reach).
    holding: Option<(String, Instant, Instant)>,
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Downed {
    pub fn new(me: impl Into<String>) -> Self {
        Downed { me: me.into(), phase: Phase::Up, actor: 0, peers: HashMap::new(), was_down: HashMap::new(), revived_by: None, revive_cmd: None, holding: None }
    }

    pub fn is_downed(&self) -> bool {
        matches!(self.phase, Phase::Downed { .. })
    }

    /// The status line this player sends.
    pub fn status(&self, pos: Option<[f32; 3]>) -> Msg {
        Msg::Status { name: self.me.clone(), downed: !matches!(self.phase, Phase::Up), pos }
    }

    /// A relay message from another player.
    pub fn receive(&mut self, now: Instant, m: Msg) {
        match m {
            Msg::Status { name, downed, pos } if name != self.me => {
                self.peers.insert(name, (now, downed, pos));
            }
            Msg::Reviving { by, target, percent } if target == self.me => self.revived_by = Some((by, percent, now)),
            Msg::Revive { by, target } if target == self.me => self.revive_cmd = Some(by),
            _ => {}
        }
    }

    fn fresh_peers(&self, now: Instant) -> impl Iterator<Item = (&String, bool, Option<[f32; 3]>)> {
        self.peers.iter().filter(move |(_, (t, ..))| now.saturating_duration_since(*t) <= PEER_STALE).map(|(n, (_, d, p))| (n, *d, *p))
    }

    /// One step. `life` = Link's health if known, `pos` = his position, `key` = the revive key is held.
    /// Returns what to write into the game, notices, relay messages to send, and the overlay.
    pub fn tick(&mut self, now: Instant, b: &Block, life: Option<i32>, pos: Option<[f32; 3]>, key: bool) -> (Vec<Write>, Vec<Notice>, Vec<Msg>, Overlay) {
        let (mut w, mut n, mut out) = (Vec::new(), Vec::new(), Vec::new());

        // Friends going down / coming back up (edge-detected on fresh statuses).
        let down_now: Vec<(String, bool)> = self.fresh_peers(now).map(|(name, d, _)| (name.clone(), d)).collect();
        for (name, d) in &down_now {
            let was = self.was_down.insert(name.clone(), *d).unwrap_or(false);
            if *d && !was {
                n.push(Notice::FriendDowned { name: name.clone() });
            } else if !*d && was {
                n.push(Notice::FriendUp { name: name.clone() });
            }
        }

        // A new Link actor = a (re)load: whatever happened before is over. `b` still shows the old flag this tick.
        let reloaded = b.self_actor != 0 && self.actor != 0 && b.self_actor != self.actor;
        if reloaded {
            if b.state != 0 {
                w.push(Write::State(0));
            }
            self.phase = Phase::Up;
        }
        if b.self_actor != 0 {
            self.actor = b.self_actor;
        }
        if self.revived_by.as_ref().is_some_and(|(_, _, t)| now.saturating_duration_since(*t) > Duration::from_secs(1)) {
            self.revived_by = None;
        }

        // Someone may still be standing: a fresh friend who is up, or one who went silent only recently.
        let silent_recently = self.peers.values().any(|(t, ..)| {
            let age = now.saturating_duration_since(*t);
            age > PEER_STALE && age <= FRIEND_SILENT
        });
        let anyone_up = down_now.iter().any(|(_, d)| !d) || silent_recently;
        match self.phase {
            Phase::Up => {
                self.revive_cmd = None;
                if b.mode != 1 {
                    w.push(Write::Mode(1)); // arm (the patch starts at 0 = vanilla)
                }
                // The patch holds a downed Link at exactly 1 (1/4 heart). A raised flag with any other health is stale:
                // left over from a game over or reload, or from a sidecar that stopped mid-way (seen live: it made
                // a restarted sidecar declare everyone down and kill both players).
                if b.state != 0 && !reloaded && life == Some(1) {
                    self.phase = Phase::Downed { alone_since: (!anyone_up).then_some(now) };
                    self.holding = None;
                    n.push(Notice::Downed);
                } else if b.state != 0 && !reloaded {
                    w.push(Write::State(0));
                }
            }
            Phase::Downed { alone_since } => {
                if b.state == 0 {
                    self.phase = Phase::Up; // cleared from outside
                } else if let Some(by) = self.revive_cmd.take() {
                    w.push(Write::Life(REVIVE_LIFE));
                    w.push(Write::State(0));
                    n.push(Notice::Revived { by });
                    self.phase = Phase::Up;
                    self.revived_by = None;
                } else {
                    if life.is_some_and(|l| l > 1) {
                        w.push(Write::Life(1)); // no eating your way back up
                    }
                    let alone_since = if anyone_up { None } else { Some(alone_since.unwrap_or(now)) };
                    if alone_since.is_some_and(|t| now.saturating_duration_since(t) >= ALL_DOWN_GRACE) {
                        // Clearing the flag also unfreezes the controls, so the Game Over screen can be answered.
                        w.push(Write::Mode(2));
                        w.push(Write::State(0));
                        w.push(Write::Life(0));
                        n.push(Notice::AllDown);
                        self.phase = Phase::Dying;
                    } else {
                        self.phase = Phase::Downed { alone_since };
                    }
                }
            }
            Phase::Dying => {
                // Back with health after the Game Over reload, or saved by a fairy. (The flag was cleared on the way in.)
                if life.is_some_and(|l| l > 1) {
                    if b.state != 0 {
                        w.push(Write::State(0));
                    }
                    w.push(Write::Mode(1));
                    self.phase = Phase::Up;
                }
            }
        }

        // Helping: the nearest downed friend within reach, while I'm up. Once a hold has started it survives short
        // blips (key read as up for a tick, a step back) of up to REVIVE_GRACE, and the friend may drift out to
        // REVIVE_KEEP_RADIUS: in the first two-PC test a strict check reset the hold every second or so.
        let mut prompt = None;
        if self.phase == Phase::Up {
            let held_target = self.holding.as_ref().map(|h| h.0.clone());
            let target = pos.and_then(|me| {
                let reach = |name: &String| if held_target.as_ref() == Some(name) { REVIVE_KEEP_RADIUS } else { REVIVE_RADIUS };
                let mut near: Vec<(String, f32)> = self
                    .fresh_peers(now)
                    .filter(|(_, d, _)| *d)
                    .filter_map(|(name, _, p)| p.map(|p| (name.clone(), dist(p, me))))
                    .filter(|(name, d)| *d <= reach(name))
                    .collect();
                // Keep the friend we are already reviving, else the nearest.
                near.sort_by(|a, b| (held_target.as_ref() != Some(&a.0)).cmp(&(held_target.as_ref() != Some(&b.0))).then(a.1.total_cmp(&b.1)));
                near.into_iter().next().map(|(name, _)| name)
            });
            if let (Some(t), true) = (&target, key) {
                let started = match &self.holding {
                    Some((h, s, _)) if h == t => *s,
                    _ => now,
                };
                self.holding = Some((t.clone(), started, now));
            } else if self.holding.as_ref().is_some_and(|(_, _, ok)| now.saturating_duration_since(*ok) > REVIVE_GRACE) {
                self.holding = None;
            }
            match self.holding.clone() {
                Some((t, started, _)) => {
                    let held = now.saturating_duration_since(started);
                    if held >= REVIVE_HOLD {
                        out.push(Msg::Revive { by: self.me.clone(), target: t.clone() });
                        n.push(Notice::RevivedFriend { name: t.clone() });
                        self.holding = None;
                        prompt = Some((t, 100));
                    } else {
                        let pct = (held.as_millis() * 100 / REVIVE_HOLD.as_millis()) as u8;
                        out.push(Msg::Reviving { by: self.me.clone(), target: t.clone(), percent: pct });
                        prompt = Some((t, pct));
                    }
                }
                None => prompt = target.map(|t| (t, 0)),
            }
        } else {
            self.holding = None;
        }

        let mut friends_down: Vec<String> = down_now.into_iter().filter(|(_, d)| *d).map(|(n, _)| n).collect();
        friends_down.sort();
        let overlay = Overlay {
            downed: !matches!(self.phase, Phase::Up),
            revived_by: self.revived_by.as_ref().map(|(by, p, _)| (by.clone(), *p)),
            prompt,
            friends_down,
        };
        (w, n, out, overlay)
    }
}

/// The relay: every line one client sends goes to every other client. Runs on the host at 127.0.0.1:[`RELAY_PORT`];
/// friends reach it through the tunnel. Lines over 1 KB are dropped. Ends when the listener fails.
pub async fn run_relay(listener: tokio::net::TcpListener) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (tx, _) = tokio::sync::broadcast::channel::<(u64, String)>(256);
    let mut next = 0u64;
    while let Ok((sock, _)) = listener.accept().await {
        next += 1;
        let id = next;
        let tx = tx.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let (r, mut wr) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(l)) if l.len() <= 1024 => { let _ = tx.send((id, l)); }
                        Ok(Some(_)) => {}
                        _ => break,
                    },
                    msg = rx.recv() => match msg {
                        Ok((from, l)) if from != id => {
                            if wr.write_all(format!("{l}\n").as_bytes()).await.is_err() { break; }
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(_) => break,
                    },
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(state: u32, mode: u32, actor: u32) -> Block {
        Block { mode, state, self_actor: actor, life_ptr: 0x1000, ..Default::default() }
    }
    fn status(name: &str, downed: bool, pos: [f32; 3]) -> Msg {
        Msg::Status { name: name.into(), downed, pos: Some(pos) }
    }
    const O: [f32; 3] = [0.0; 3];

    #[test]
    fn parses_the_block() {
        let mut b = vec![0u8; BLOCK_LEN];
        b[..4].copy_from_slice(&MAGIC.to_be_bytes());
        b[4..8].copy_from_slice(&1u32.to_be_bytes());
        b[28..32].copy_from_slice(&(-3i32).to_be_bytes());
        b[32..36].copy_from_slice(&0x6ed1_32b8u32.to_be_bytes());
        let p = Block::parse(&b).unwrap();
        assert_eq!((p.mode, p.last_life, p.self_actor), (1, -3, 0x6ed1_32b8));
        b[0] = 0;
        assert!(Block::parse(&b).is_none());
        assert!(Block::parse(&[0; 8]).is_none());
    }

    #[test]
    fn reads_base_and_codecave_from_the_cemu_log() {
        let log = "[22:55:01] Init Wii U memory space (base: 0x0000021e40000000)
                   [22:55:11] Applying patch group 'BotW_BOTWMFix_V208' (Codecave: 01808c00-018091e8)
                   [22:55:11] Applying patch group 'BotW_PrivCordDowned_V208' (Codecave: 0180c800-0180c8a8)
";
        assert_eq!(parse_cemu_log(log), Some((0x21e4000_0000, 0x0180_c800)));
        assert_eq!(parse_cemu_log("[22:55:01] Init Wii U memory space (base: 0x0000021e40000000)"), None);
    }

    #[test]
    fn messages_are_tagged_json() {
        let s = serde_json::to_string(&Msg::Reviving { by: "a".into(), target: "b".into(), percent: 40 }).unwrap();
        assert_eq!(s, r#"{"t":"reviving","by":"a","target":"b","percent":40}"#);
        assert_eq!(serde_json::from_str::<Msg>(&s).unwrap(), Msg::Reviving { by: "a".into(), target: "b".into(), percent: 40 });
    }

    /// Two players, Kev (downed) and blackz (helper), wired through each other's `receive`.
    #[test]
    fn helper_holds_the_key_for_12s_and_the_downed_player_gets_up() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        let mut bz = Downed::new("blackz");
        let at = |s: f32| t0 + Duration::from_secs_f32(s);

        // Arming.
        let (w, ..) = kev.tick(t0, &block(0, 0, 7), Some(20), Some(O), false);
        assert_eq!(w, vec![Write::Mode(1)]);

        // Kev goes down; blackz is up, 10 m away.
        bz.receive(at(0.1), kev.status(Some(O)));
        kev.receive(at(0.1), status("blackz", false, [10.0, 0.0, 0.0]));
        let (_, n, _, o) = kev.tick(at(0.2), &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(n, vec![Notice::Downed]);
        assert!(o.downed && kev.is_downed());
        bz.receive(at(0.3), kev.status(Some(O)));
        let (_, n, out, o) = bz.tick(at(0.3), &block(0, 1, 9), Some(20), Some([10.0, 0.0, 0.0]), true);
        assert_eq!(n, vec![Notice::FriendDowned { name: "Kev".into() }]);
        assert!(out.is_empty() && o.prompt.is_none(), "too far: no prompt");
        assert_eq!(o.friends_down, vec!["Kev".to_string()]);

        // Eating while down is undone, and Kev stays down while a friend is up (no timer).
        kev.receive(at(30.0), status("blackz", false, [10.0, 0.0, 0.0]));
        let (w, ..) = kev.tick(at(30.0), &block(1, 1, 7), Some(16), Some(O), false);
        assert_eq!(w, vec![Write::Life(1)]);

        // blackz walks up: prompt at 0 %; holds G.
        let near = Some([1.0, 0.5, 1.0]);
        bz.receive(at(31.0), kev.status(Some(O)));
        let (_, _, out, o) = bz.tick(at(31.0), &block(0, 1, 9), Some(20), near, false);
        assert!(out.is_empty());
        assert_eq!(o.prompt, Some(("Kev".into(), 0)));
        bz.receive(at(31.5), kev.status(Some(O)));
        let (_, _, out, o) = bz.tick(at(31.5), &block(0, 1, 9), Some(20), near, true);
        assert_eq!(o.prompt, Some(("Kev".into(), 0)));
        for m in out {
            kev.receive(at(31.5), m);
        }
        let (.., o) = kev.tick(at(31.6), &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(o.revived_by, Some(("blackz".into(), 0)));
        bz.receive(at(37.5), kev.status(Some(O)));
        let (_, _, _, o) = bz.tick(at(37.5), &block(0, 1, 9), Some(20), near, true);
        assert_eq!(o.prompt, Some(("Kev".into(), 50)));

        // A blip (key up for one tick) doesn't reset.
        bz.receive(at(37.6), kev.status(Some(O)));
        bz.tick(at(37.6), &block(0, 1, 9), Some(20), near, false);
        bz.receive(at(37.7), kev.status(Some(O)));
        let (.., o) = bz.tick(at(37.7), &block(0, 1, 9), Some(20), near, true);
        assert_eq!(o.prompt, Some(("Kev".into(), 51)));

        // Letting go for longer resets.
        bz.receive(at(38.0), kev.status(Some(O)));
        bz.tick(at(38.0), &block(0, 1, 9), Some(20), near, false);
        bz.receive(at(38.5), kev.status(Some(O)));
        bz.tick(at(38.5), &block(0, 1, 9), Some(20), near, false);
        bz.receive(at(38.8), kev.status(Some(O)));
        let (.., o) = bz.tick(at(38.8), &block(0, 1, 9), Some(20), near, true);
        assert_eq!(o.prompt, Some(("Kev".into(), 0)));

        // 12 s of holding finishes the revive.
        let mut done = Vec::new();
        for i in 1..=13 {
            let t = at(38.8 + i as f32);
            bz.receive(t, kev.status(Some(O)));
            let (_, n, out, _) = bz.tick(t, &block(0, 1, 9), Some(20), near, true);
            if n.contains(&Notice::RevivedFriend { name: "Kev".into() }) {
                done = out;
                break;
            }
        }
        assert_eq!(done, vec![Msg::Revive { by: "blackz".into(), target: "Kev".into() }]);
        for m in done {
            kev.receive(at(51.0), m);
        }
        kev.receive(at(51.0), status("blackz", false, [1.0, 0.5, 1.0]));
        let (w, n, ..) = kev.tick(at(51.1), &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(w, vec![Write::Life(REVIVE_LIFE), Write::State(0)]);
        assert_eq!(n, vec![Notice::Revived { by: "blackz".into() }]);
        assert!(!kev.is_downed());
    }

    #[test]
    fn a_started_revive_survives_drifting_to_4m_but_not_beyond() {
        let t0 = Instant::now();
        let mut bz = Downed::new("blackz");
        let at = |s: f32| t0 + Duration::from_secs_f32(s);
        let tick = |bz: &mut Downed, s: f32, x: f32| {
            bz.receive(at(s), status("Kev", true, O));
            bz.tick(at(s), &block(0, 1, 9), Some(20), Some([x, 0.0, 0.0]), true).3.prompt
        };
        assert_eq!(tick(&mut bz, 0.0, 4.0), None, "4 m is too far to start");
        assert_eq!(tick(&mut bz, 1.0, 2.0), Some(("Kev".into(), 0)));
        assert_eq!(tick(&mut bz, 7.0, 4.2), Some(("Kev".into(), 50)), "drifting to 4.2 m keeps going");
        assert_eq!(tick(&mut bz, 7.5, 6.0), Some(("Kev".into(), 54)), "a short step away is a blip");
        assert_eq!(tick(&mut bz, 8.5, 6.0), None, "staying away ends it");
        assert_eq!(tick(&mut bz, 9.0, 2.0), Some(("Kev".into(), 0)), "and it starts over");
    }

    #[test]
    fn a_stale_downed_flag_is_cleared_not_obeyed() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        // Fresh sidecar, flag still up from before a reload, full health: clear it, don't go down (or kill anyone).
        let (w, n, ..) = kev.tick(t0, &block(1, 1, 7), Some(12), Some(O), false);
        assert_eq!(w, vec![Write::State(0)]);
        assert!(n.is_empty() && !kev.is_downed());
        // Dead (0) with the flag up: also stale.
        let (w, ..) = kev.tick(t0 + ALL_DOWN_GRACE * 2, &block(1, 1, 7), Some(0), Some(O), false);
        assert_eq!(w, vec![Write::State(0)]);
        assert!(!kev.is_downed());
    }

    #[test]
    fn a_revive_for_someone_else_is_ignored() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        kev.receive(t0, status("blackz", false, O));
        kev.tick(t0, &block(1, 1, 7), Some(1), Some(O), false);
        kev.receive(t0, Msg::Revive { by: "blackz".into(), target: "Someone".into() });
        let (w, ..) = kev.tick(t0 + Duration::from_millis(100), &block(1, 1, 7), Some(1), Some(O), false);
        assert!(w.is_empty());
        assert!(kev.is_downed());
    }

    #[test]
    fn everyone_down_means_game_over_for_everyone() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        kev.receive(t0, status("blackz", true, O));
        kev.tick(t0, &block(1, 1, 7), Some(1), Some(O), false);
        // Still inside the grace period.
        kev.receive(t0 + Duration::from_secs(2), status("blackz", true, O));
        let (w, ..) = kev.tick(t0 + Duration::from_secs(2), &block(1, 1, 7), Some(1), Some(O), false);
        assert!(w.is_empty());
        kev.receive(t0 + Duration::from_secs(3), status("blackz", true, O));
        let (w, n, ..) = kev.tick(t0 + Duration::from_secs(3), &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(w, vec![Write::Mode(2), Write::State(0), Write::Life(0)]);
        assert_eq!(n, vec![Notice::AllDown]);
        // Death and reload: a new Link actor re-arms.
        let (w, ..) = kev.tick(t0 + Duration::from_secs(12), &block(1, 2, 8), Some(12), Some(O), false);
        assert_eq!(w, vec![Write::State(0), Write::Mode(1)]);
        assert!(!kev.is_downed());
    }

    #[test]
    fn alone_or_friend_gone_means_game_over() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        kev.tick(t0, &block(1, 1, 7), Some(1), Some(O), false);
        let (w, ..) = kev.tick(t0 + ALL_DOWN_GRACE, &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(w, vec![Write::Mode(2), Write::State(0), Write::Life(0)]);

        // A friend who goes silent still counts as standing for FRIEND_SILENT (their PrivCord may be restarting), then
        // no longer.
        let mut kev = Downed::new("Kev");
        kev.receive(t0, status("blackz", false, O));
        kev.tick(t0, &block(1, 1, 7), Some(1), Some(O), false);
        let (w, ..) = kev.tick(t0 + Duration::from_secs(10), &block(1, 1, 7), Some(1), Some(O), false);
        assert!(w.is_empty(), "silent for 10 s: still waiting");
        kev.tick(t0 + Duration::from_secs(31), &block(1, 1, 7), Some(1), Some(O), false);
        let (w, ..) = kev.tick(t0 + Duration::from_secs(34), &block(1, 1, 7), Some(1), Some(O), false);
        assert_eq!(w, vec![Write::Mode(2), Write::State(0), Write::Life(0)]);
    }

    #[test]
    fn a_fairy_while_dying_rearms() {
        let t0 = Instant::now();
        let mut kev = Downed::new("Kev");
        kev.tick(t0, &block(1, 1, 7), Some(1), None, false);
        kev.tick(t0 + ALL_DOWN_GRACE, &block(1, 1, 7), Some(1), None, false);
        let (w, ..) = kev.tick(t0 + Duration::from_secs(5), &block(1, 2, 7), Some(20), None, false);
        assert_eq!(w, vec![Write::State(0), Write::Mode(1)]);
    }

    #[tokio::test]
    async fn relay_forwards_lines_to_the_others_only() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(run_relay(l));
        let a = tokio::net::TcpStream::connect(addr).await.unwrap();
        let b = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (ar, mut aw) = a.into_split();
        let (br, mut bw) = b.into_split();
        let line = serde_json::to_string(&status("Kev", true, [1.0, 2.0, 3.0])).unwrap();
        aw.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        let mut bl = BufReader::new(br).lines();
        let got = tokio::time::timeout(Duration::from_secs(2), bl.next_line()).await.unwrap().unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Msg>(&got).unwrap(), status("Kev", true, [1.0, 2.0, 3.0]));
        bw.write_all(b"hello\n").await.unwrap();
        let mut al = BufReader::new(ar).lines();
        let got = tokio::time::timeout(Duration::from_secs(2), al.next_line()).await.unwrap().unwrap().unwrap();
        assert_eq!(got, "hello", "A gets B's line, not its own echo");
    }
}

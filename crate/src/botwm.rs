//! Co-op helpers for Breath of the Wild Multiplayer (the BOTWM 1.0.4 mod) when PrivCord hosts its dedicated
//! server. Everything here drives the mod's own, unmodified server through the two doors it already has:
//!
//! - **`ServerConfig.ini`**, read once at start: the `[DefaultGamemode]` section holds the sync toggles and the
//!   special mode (`Special=0/1/2`). [`apply_settings`] rewrites it.
//! - **The server console** (its stdin/stdout, which PrivCord holds): `Tp p1 p2` queues a teleport that the
//!   client applies on its next update, `DeathSwap on` arms Death Swap, and the debug command `_Get <n> <field>`
//!   prints a player's server-side state. [`Party`] reads the console output back into a party list.
//!
//! The BOTWM wire protocol has no free-text or map-pin channel, so nothing here sends game messages; PrivCord
//! shows its own notices instead (see `docs/HANDOFF-BOTW-MP-COOP.md`).
//!
//! [`serve_party`] is the small "party feed" PrivCord's host runs on [`PARTY_PORT`] next to the game server, so
//! friends who joined through the tunnel get the same party list and can ask for teleports.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
};

/// TCP port of the party feed on the host (127.0.0.1), tunnelled next to the game server's 5050.
pub const PARTY_PORT: u16 = 5051;
/// The server's player limit; player numbers are 0..32 (the console shows them as 1..=32).
pub const PLAYER_LIMIT: u8 = 32;
/// Largest party-feed reply a friend accepts (32 players fit in a few KiB).
const MAX_REPLY: usize = 64 * 1024;
/// BOTWM names travel as a length byte, so 255 bytes is the most a real one can be.
const MAX_NAME: usize = 255;

/// The server's special game modes (`Special=` in `ServerConfig.ini`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Plain co-op: everyone explores the same Hyrule.
    #[default]
    Coop,
    /// One speedrunner, everyone else hunts them (`Special=1`).
    HunterVsSpeedrunner,
    /// Players 1 and 2 swap places every few minutes (`Special=2`).
    DeathSwap,
}

impl Mode {
    pub fn special(self) -> u8 {
        match self {
            Mode::Coop => 0,
            Mode::HunterVsSpeedrunner => 1,
            Mode::DeathSwap => 2,
        }
    }

    /// The name the server reports for the session (`Name=`, shown in the mod's lobby browser).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Coop => "PrivCord co-op",
            Mode::HunterVsSpeedrunner => "Hunter vs Speedrunner",
            Mode::DeathSwap => "Death Swap",
        }
    }
}

/// What the host chose before hosting. Field names follow PrivCord's UI; [`apply_settings`] maps them onto the
/// server's keys (`QuestSync` is the story-quest sync, `DungeonSync` the Divine Beasts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CoopSettings {
    pub enemy_sync: bool,
    pub story_sync: bool,
    pub korok_sync: bool,
    pub tower_sync: bool,
    pub shrine_sync: bool,
    pub location_sync: bool,
    pub dungeon_sync: bool,
    pub mode: Mode,
    /// Death Swap only: the random swap interval, in minutes (the server's default is 5 to 10).
    pub swap_min: u16,
    pub swap_max: u16,
    /// PrivCord's own "Shared enemies (beta)": enemies near two players are run by the nearest one's game and shown
    /// as copies elsewhere (`botw_enemies`). Not a server setting; off by default, and older apps ignore it.
    pub shared_enemies: bool,
}

impl Default for CoopSettings {
    /// The mod's own "Game Completion" preset: shared enemies and map progress, but each player keeps their own
    /// story and Divine Beasts (story sync can complete quests out of order on the other save).
    fn default() -> Self {
        Self {
            enemy_sync: true,
            story_sync: false,
            korok_sync: true,
            tower_sync: true,
            shrine_sync: true,
            location_sync: true,
            dungeon_sync: false,
            mode: Mode::Coop,
            swap_min: 5,
            swap_max: 10,
            shared_enemies: false,
        }
    }
}

impl CoopSettings {
    /// The `[DefaultGamemode]` keys and values, in the file's own order.
    fn ini_values(&self) -> [(&'static str, String); 9] {
        let b = |v: bool| if v { "True" } else { "False" }.to_owned();
        [
            ("Name", self.mode.label().to_owned()),
            ("EnemySync", b(self.enemy_sync)),
            ("QuestSync", b(self.story_sync)),
            ("KorokSync", b(self.korok_sync)),
            ("TowerSync", b(self.tower_sync)),
            ("ShrineSync", b(self.shrine_sync)),
            ("LocationSync", b(self.location_sync)),
            ("DungeonSync", b(self.dungeon_sync)),
            ("Special", self.mode.special().to_string()),
        ]
    }

    /// Console commands to send once the server is up, for settings the ini can't hold.
    pub fn startup_commands(&self) -> Vec<String> {
        match self.mode {
            // The server keeps Death Swap disabled until `DeathSwap on`, even with Special=2.
            Mode::DeathSwap => {
                let (lo, hi) = (self.swap_min.max(1), self.swap_max.max(1));
                vec![format!("DeathSwap {}:{}", lo.min(hi), lo.max(hi)), "DeathSwap on".to_owned()]
            }
            _ => Vec::new(),
        }
    }
}

/// Rewrite the server's `ServerConfig.ini` so it starts unattended, on loopback, with `settings`:
/// `[Gamemode] DefaultGamemode=True` (skip the interactive prompt), `[Connection] IP=localhost` → `127.0.0.1`
/// (the stock value binds a LAN interface the tunnel can't reach; a custom IP is kept), and every
/// `[DefaultGamemode]` key set from `settings`. Comments, other keys and line endings are kept; missing keys
/// and sections are added. The server parses each value with `Convert.ChangeType`, so booleans are
/// `True`/`False`, and it throws on a missing key, which is why all nine are always written.
pub fn apply_settings(ini: &str, settings: &CoopSettings) -> String {
    let nl = if ini.contains("\r\n") { "\r\n" } else { "\n" };
    let values = settings.ini_values();
    let mut out: Vec<String> = Vec::new();
    let mut section = String::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut had_gamemode_key = false;
    let mut had_section = false;
    // Append keys the current section is missing before the next section starts.
    let close = |section: &str, out: &mut Vec<String>, seen: &[&str], had_gamemode_key: bool| {
        // Insert before trailing blank lines so the file keeps its spacing.
        let mut at = out.len();
        while at > 0 && out[at - 1].trim().is_empty() {
            at -= 1;
        }
        let mut add = Vec::new();
        if section.eq_ignore_ascii_case("DefaultGamemode") {
            for (k, v) in &values {
                if !seen.contains(k) {
                    add.push(format!("{k}={v}"));
                }
            }
        } else if section.eq_ignore_ascii_case("Gamemode") && !had_gamemode_key {
            add.push("DefaultGamemode=True".to_owned());
        }
        for (i, line) in add.into_iter().enumerate() {
            out.insert(at + i, line);
        }
    };
    for raw in ini.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            close(&section, &mut out, &seen, had_gamemode_key);
            section = trimmed[1..trimmed.len() - 1].trim().to_owned();
            seen.clear();
            had_gamemode_key = false;
            had_section |= section.eq_ignore_ascii_case("DefaultGamemode");
            out.push(line.to_owned());
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=').filter(|_| !trimmed.starts_with('#') && !trimmed.starts_with(';')) else {
            out.push(line.to_owned());
            continue;
        };
        let key = key.trim();
        let new = if section.eq_ignore_ascii_case("Connection") && key.eq_ignore_ascii_case("IP") && value.trim().eq_ignore_ascii_case("localhost") {
            Some("127.0.0.1".to_owned())
        } else if section.eq_ignore_ascii_case("Gamemode") && key.eq_ignore_ascii_case("DefaultGamemode") {
            had_gamemode_key = true;
            Some("True".to_owned())
        } else if section.eq_ignore_ascii_case("DefaultGamemode") {
            values.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(k, v)| {
                seen.push(k);
                v.clone()
            })
        } else {
            None
        };
        match new {
            Some(v) => out.push(format!("{key}={v}")),
            None => out.push(line.to_owned()),
        }
    }
    close(&section, &mut out, &seen, had_gamemode_key);
    if !had_section {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push("[DefaultGamemode]".to_owned());
        out.extend(values.iter().map(|(k, v)| format!("{k}={v}")));
    }
    let mut text = out.join(nl);
    if ini.ends_with('\n') {
        text.push_str(nl);
    }
    text
}

/// `Tp p<from> p<to>`: teleport player `from` to player `to` (0-based numbers; the console counts from 1).
/// Player numbers instead of names, because the console splits arguments on spaces.
pub fn teleport_command(from: u8, to: u8) -> String {
    format!("Tp p{} p{}", from + 1, to + 1)
}

/// `_Get <n> <field>`: the server prints `---Player<n>'s <field> is---` and then the value.
pub fn get_command(player: u8, field: Field) -> String {
    format!("_Get {} {}", player + 1, field.name())
}

/// The server-side player fields the party list polls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Position,
    Health,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Position => "Position",
            Field::Health => "Health",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "position" => Some(Field::Position),
            "health" => Some(Field::Health),
            _ => None,
        }
    }
}

/// One player as the server sees them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyMember {
    /// 0-based server player number (the console and the mod's UI show it + 1).
    pub number: u8,
    pub name: String,
    /// World position in metres, if the server has one yet (it reports (0,0,0) until the first update).
    pub position: Option<[f32; 3]>,
    /// The server's `Health` field as the client reports it (not verified in game: likely quarter hearts).
    pub health: Option<i32>,
    /// Times the server had to wait for more bytes of this player's message ("Retried N times"): a rough
    /// connection-quality signal. Over PrivCord's framed tunnel this should stay near 0.
    pub retries: u32,
    /// Messages the server could not parse ("Error receiving message").
    pub errors: u32,
}

/// What a console line meant, for the caller's own log and notices.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Joined { number: u8, name: String },
    Left { number: u8, name: String },
}

/// The party, rebuilt from the dedicated server's console output (one call per line).
#[derive(Debug, Default)]
pub struct Party {
    members: BTreeMap<u8, PartyMember>,
    /// A `_Get` header was seen; the next timestamped line is its value.
    pending: Option<(u8, Field)>,
}

/// Strip the server's `[HH:mm:ss] ` prefix. Lines without it are continuation lines (exception stack traces).
fn message(line: &str) -> Option<&str> {
    let b = line.as_bytes();
    let stamped = b.len() >= 11
        && b[0] == b'['
        && b[3] == b':'
        && b[6] == b':'
        && b[9] == b']'
        && b[10] == b' '
        && [1, 2, 4, 5, 7, 8].iter().all(|&i| b[i].is_ascii_digit());
    stamped.then(|| line[11..].trim_end())
}

/// Parse `[x, y, z]`. .NET prints floats with the server's culture, so the decimal mark may be a comma; the
/// separator between numbers is always ", ".
fn parse_vec3(s: &str) -> Option<[f32; 3]> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let parts: Vec<f32> = inner.split(", ").map(|p| p.trim().replace(',', ".").parse().ok()).collect::<Option<_>>()?;
    <[f32; 3]>::try_from(parts).ok()
}

impl Party {
    pub fn members(&self) -> Vec<PartyMember> {
        self.members.values().cloned().collect()
    }

    pub fn numbers(&self) -> Vec<u8> {
        self.members.keys().copied().collect()
    }

    pub fn contains(&self, number: u8) -> bool {
        self.members.contains_key(&number)
    }

    /// Read one line of console output. Returns what it meant (if anything) and whether the line is a reply to
    /// one of PrivCord's own polls (`_Get`), which the caller keeps out of the server log.
    pub fn feed(&mut self, line: &str) -> (Option<Event>, bool) {
        let Some(msg) = message(line) else { return (None, false) };
        // The value line of a `_Get`. Client threads log too, so another line can land in between: a line that
        // isn't a value of the expected shape drops the pending reply and is read normally below.
        if let Some((n, field)) = self.pending.take() {
            let applied = match field {
                Field::Position => parse_vec3(msg).map(|p| {
                    if let Some(m) = self.members.get_mut(&n) {
                        m.position = Some(p).filter(|p| p.iter().any(|v| *v != 0.0));
                    }
                }),
                Field::Health => msg.trim().parse::<i32>().ok().map(|h| {
                    if let Some(m) = self.members.get_mut(&n) {
                        m.health = Some(h);
                    }
                }),
            };
            if applied.is_some() {
                return (None, true);
            }
        }
        // ---Player2's Position is---
        if let Some(rest) = msg.strip_prefix("---Player").and_then(|r| r.strip_suffix(" is---")) {
            if let Some((n, field)) = rest.split_once("'s ") {
                if let (Ok(n), Some(field)) = (n.parse::<u8>(), Field::parse(field)) {
                    if (1..=PLAYER_LIMIT).contains(&n) {
                        self.pending = Some((n - 1, field));
                        return (None, true);
                    }
                }
            }
        }
        // Player Kev joined the server. Assigned to player 1.
        if let Some(rest) = msg.strip_prefix("Player ").and_then(|r| r.strip_suffix('.')) {
            if let Some((name, n)) = rest.rsplit_once(" joined the server. Assigned to player ") {
                if let Ok(n) = n.parse::<u8>() {
                    if (1..=PLAYER_LIMIT).contains(&n) {
                        let number = n - 1;
                        let member = PartyMember { number, name: name.to_owned(), position: None, health: None, retries: 0, errors: 0 };
                        self.members.insert(number, member);
                        return (Some(Event::Joined { number, name: name.to_owned() }), false);
                    }
                }
            }
        }
        // Player 2 is not connected (reply to a poll of someone who left without a disconnect line).
        if let Some(n) = msg.strip_prefix("Player ").and_then(|r| r.strip_suffix(" is not connected")).and_then(|n| n.parse::<u8>().ok()) {
            let left = n.checked_sub(1).and_then(|number| self.members.remove(&number));
            return (left.map(|m| Event::Left { number: m.number, name: m.name }), true);
        }
        // Player Kev disconnected because System.ApplicationException: ...
        if let Some((name, _)) = msg.strip_prefix("Player ").and_then(|r| r.split_once(" disconnected because ")) {
            let number = self.members.values().find(|m| m.name == name).map(|m| m.number);
            if let Some(m) = number.and_then(|n| self.members.remove(&n)) {
                return (Some(Event::Left { number: m.number, name: m.name }), false);
            }
            return (None, false);
        }
        // [Kev] Retried 3 times and took 12 milliseconds / [Kev] Error receiving message. Retrying...
        if let Some((name, rest)) = msg.strip_prefix('[').and_then(|r| r.split_once("] ")) {
            if let Some(m) = self.members.values_mut().find(|m| m.name == name) {
                if let Some(k) = rest.strip_prefix("Retried ").and_then(|r| r.split_once(' ')).and_then(|(k, _)| k.parse::<u32>().ok()) {
                    m.retries = m.retries.saturating_add(k);
                } else if rest.starts_with("Error receiving message") {
                    m.errors = m.errors.saturating_add(1);
                }
            }
        }
        (None, false)
    }
}

/// This PC's player number in its latest BOTWM session, from the mod client's own log
/// (`%APPDATA%\BOTWM\LatestLog.txt`, line `[INF][<date>] Assigned to player 0`; 0-based like the server).
pub fn local_player_from_log(log: &str) -> Option<u8> {
    log.lines()
        .rev()
        .find_map(|l| l.split_once("] Assigned to player ").and_then(|(_, n)| n.trim().parse::<u8>().ok()))
        .filter(|n| *n < PLAYER_LIMIT)
}

/// Horizontal distance in metres, the way the server measures it (it ignores height).
pub fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// A request on the party feed (one JSON object per line).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum FeedRequest {
    /// The party list and the host's settings.
    Party,
    /// Teleport player `from` to player `to` (0-based).
    Teleport { from: u8, to: u8 },
}

/// The host's answer to [`FeedRequest::Party`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartyView {
    pub members: Vec<PartyMember>,
    pub settings: CoopSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<PartyView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Check a teleport against the current party: both players connected, not the same player.
pub fn check_teleport(party: &Party, from: u8, to: u8) -> Result<String, String> {
    if from == to {
        return Err("That's the same player.".into());
    }
    if !party.contains(from) || !party.contains(to) {
        return Err("That player isn't in the game right now.".into());
    }
    Ok(teleport_command(from, to))
}

/// Answer one feed request. Teleports go to `console` (a line for the server's stdin).
pub async fn handle_feed(req: FeedRequest, party: &Mutex<Party>, settings: &CoopSettings, console: &mpsc::Sender<String>) -> FeedReply {
    match req {
        FeedRequest::Party => {
            let members = party.lock().await.members();
            FeedReply { ok: true, party: Some(PartyView { members, settings: *settings }), error: None }
        }
        FeedRequest::Teleport { from, to } => {
            let checked = check_teleport(&*party.lock().await, from, to);
            match checked {
                Ok(cmd) => match console.send(cmd).await {
                    Ok(()) => FeedReply { ok: true, party: None, error: None },
                    Err(_) => FeedReply { ok: false, party: None, error: Some("The game server has stopped.".into()) },
                },
                Err(e) => FeedReply { ok: false, party: None, error: Some(e) },
            }
        }
    }
}

/// Serve the party feed on `listener` until the task is dropped/aborted. Each connection may send several
/// requests, one JSON line each; lines over 4 KiB or that don't parse get an error reply.
pub async fn serve_party(listener: TcpListener, party: Arc<Mutex<Party>>, settings: CoopSettings, console: mpsc::Sender<String>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let (party, console) = (party.clone(), console.clone());
        tokio::spawn(async move {
            let _ = serve_conn(stream, &party, &settings, &console).await;
        });
    }
}

async fn serve_conn(stream: TcpStream, party: &Mutex<Party>, settings: &CoopSettings, console: &mpsc::Sender<String>) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read.take(1 << 20)).lines();
    loop {
        // An idle friend's connection closes after a minute; the UI reconnects per poll anyway.
        let line = match tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await {
            Ok(Ok(Some(line))) => line,
            _ => return Ok(()),
        };
        let reply = match serde_json::from_str::<FeedRequest>(&line).ok().filter(|_| line.len() <= 4096) {
            Some(req) => handle_feed(req, party, settings, console).await,
            None => FeedReply { ok: false, party: None, error: Some("bad request".into()) },
        };
        let mut out = serde_json::to_vec(&reply).unwrap_or_default();
        out.push(b'\n');
        write.write_all(&out).await?;
    }
}

/// Ask a party feed (the host's, or a joiner's tunnelled copy of it at 127.0.0.1:[`PARTY_PORT`]).
pub async fn ask_feed(addr: std::net::SocketAddr, req: &FeedRequest) -> anyhow::Result<FeedReply> {
    let fut = async {
        let mut stream = TcpStream::connect(addr).await?;
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        stream.write_all(&line).await?;
        // The host is a friend's PC, not trusted for size: one reply line, at most MAX_REPLY bytes, and the party
        // it describes is capped to the server's limits before the UI sees it.
        let mut reader = BufReader::new(stream.take(MAX_REPLY as u64));
        let mut reply = String::new();
        reader.read_line(&mut reply).await?;
        anyhow::ensure!(!reply.is_empty(), "the host closed the party feed");
        anyhow::ensure!(reply.ends_with('\n'), "the host's party list is too long");
        let mut reply: FeedReply = serde_json::from_str(&reply)?;
        if let Some(party) = reply.party.as_mut() {
            party.members.truncate(PLAYER_LIMIT as usize);
            for m in &mut party.members {
                if m.name.len() > MAX_NAME {
                    let cut = (0..=MAX_NAME).rev().find(|&i| m.name.is_char_boundary(i)).unwrap_or(0);
                    m.name.truncate(cut);
                }
            }
        }
        Ok::<_, anyhow::Error>(reply)
    };
    tokio::time::timeout(Duration::from_secs(5), fut).await.map_err(|_| anyhow::anyhow!("the host didn't answer"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ServerConfig.ini with the stock layout (sections, keys, default values) and our own comment lines.
    const STOCK: &str = "[Connection]\r\nIP=localhost\r\nPort=5050\r\nPassword=\r\n\r\n[ServerInformation]\r\nDescription=Test server\r\n\r\n[Gamemode]\r\n# test comment: kept as is\r\nDefaultGamemode=False\r\n\r\n[DefaultGamemode]\r\nName=Custom\r\nEnemySync=False\r\nQuestSync=False\r\nKorokSync=False\r\nTowerSync=False\r\nShrineSync=False\r\nLocationSync=False\r\nDungeonSync=False\r\n# another comment\r\nSpecial=0";

    #[test]
    fn settings_rewrite_the_stock_file() {
        let s = CoopSettings { mode: Mode::DeathSwap, story_sync: true, korok_sync: false, ..Default::default() };
        let out = apply_settings(STOCK, &s);
        for want in ["IP=127.0.0.1", "DefaultGamemode=True", "Name=Death Swap", "EnemySync=True", "QuestSync=True", "KorokSync=False", "TowerSync=True", "DungeonSync=False", "Special=2"] {
            assert!(out.contains(want), "missing {want} in\n{out}");
        }
        assert!(!out.contains("IP=localhost") && !out.contains("DefaultGamemode=False"));
        assert!(out.contains("# test comment: kept as is"), "comments kept");
        assert!(out.contains("Description=Test server"));
        assert!(!out.replace("\r\n", "").contains('\n'), "CRLF kept");
        assert_eq!(apply_settings(&out, &s), out, "idempotent");
        // Every key appears exactly once.
        for (k, _) in s.ini_values() {
            assert_eq!(out.matches(&format!("\r\n{k}=")).count(), 1, "{k}");
        }
    }

    #[test]
    fn settings_fill_in_missing_keys_and_sections() {
        let out = apply_settings("[Connection]\nIP=192.168.1.50\nPort=5050\n\n[DefaultGamemode]\nName=Custom\nEnemySync=False\n\n[Other]\nX=1\n", &CoopSettings::default());
        assert!(out.contains("IP=192.168.1.50"), "custom IP kept");
        let dg = out.split("[DefaultGamemode]").nth(1).unwrap().split("[Other]").next().unwrap();
        for (k, v) in CoopSettings::default().ini_values() {
            assert!(dg.contains(&format!("{k}={v}")), "{k} in DefaultGamemode section:\n{out}");
        }
        assert!(out.contains("\n\n[Other]\nX=1\n"), "spacing before the next section kept:\n{out}");
        let bare = apply_settings("[Connection]\nIP=localhost\n", &CoopSettings::default());
        assert!(bare.contains("[DefaultGamemode]\nName=PrivCord co-op\nEnemySync=True"), "{bare}");
        assert!(bare.contains("IP=127.0.0.1"));
    }

    #[test]
    fn startup_commands_arm_death_swap() {
        assert!(CoopSettings::default().startup_commands().is_empty());
        let ds = CoopSettings { mode: Mode::DeathSwap, swap_min: 8, swap_max: 3, ..Default::default() };
        assert_eq!(ds.startup_commands(), vec!["DeathSwap 3:8".to_owned(), "DeathSwap on".to_owned()]);
    }

    #[test]
    fn commands_use_one_based_numbers() {
        assert_eq!(teleport_command(1, 0), "Tp p2 p1");
        assert_eq!(get_command(0, Field::Position), "_Get 1 Position");
    }

    #[test]
    fn party_follows_the_console() {
        let mut p = Party::default();
        assert_eq!(
            p.feed("[19:50:01] Player Kev joined the server. Assigned to player 1."),
            (Some(Event::Joined { number: 0, name: "Kev".into() }), false)
        );
        p.feed("[19:50:09] Player Blackz joined the server. Assigned to player 2.");
        assert_eq!(p.numbers(), vec![0, 1]);
        assert_eq!(p.members()[1].name, "Blackz");
        // _Get replies (culture with a decimal comma) are consumed and hidden from the log.
        assert_eq!(p.feed("[19:50:10] ---Player2's Position is---"), (None, true));
        assert_eq!(p.feed("[19:50:10] [-1020,5, 252,25, 1805]"), (None, true));
        assert_eq!(p.members()[1].position, Some([-1020.5, 252.25, 1805.0]));
        p.feed("[19:50:10] ---Player1's Position is---");
        p.feed("[19:50:10] [-1000.5, 250, 1800]");
        p.feed("[19:50:10] ---Player1's Health is---");
        p.feed("[19:50:10] 52");
        assert_eq!(p.members()[0].health, Some(52));
        assert!((distance(p.members()[0].position.unwrap(), p.members()[1].position.unwrap()) - 20.6155).abs() < 0.01);
        // Zero means "no update yet".
        p.feed("[19:50:11] ---Player1's Position is---");
        p.feed("[19:50:11] [0, 0, 0]");
        assert_eq!(p.members()[0].position, None);
        // A client thread's line between a header and its value is read as itself, not as the value.
        p.feed("[19:50:11] ---Player1's Health is---");
        assert_eq!(p.feed("[19:50:12] [Blackz] Retried 3 times and took 40 milliseconds"), (None, false));
        assert_eq!(p.members()[0].health, Some(52));
        // Connection quality counters.
        p.feed("[19:50:12] [Blackz] Error receiving message. Retrying...");
        assert_eq!((p.members()[1].retries, p.members()[1].errors), (3, 1));
        // Stack trace lines (no timestamp) are ignored, also while a _Get reply is pending.
        p.feed("[19:50:13] Player Kev disconnected because System.ApplicationException: Connection lost with player.");
        assert_eq!(p.feed("   at BOTWM.Server.Server.handleClient(Socket connection)"), (None, false));
        assert_eq!(p.numbers(), vec![1]);
        // A clean disconnect logs nothing; the next poll finds out.
        assert_eq!(p.feed("[19:50:14] Player 2 is not connected"), (Some(Event::Left { number: 1, name: "Blackz".into() }), true));
        assert!(p.members().is_empty());
    }

    #[test]
    fn local_player_comes_from_the_latest_assignment() {
        let log = "Logging level: Debug\n[INF][19-10-2026 20:49:48] Assigned to player 0\n[INF][19-10-2026 21:49:48] Start of Breath of the Wild Multiplayer\n[INF][19-10-2026 21:49:48] Assigned to player 1\n[DBG][19-10-2026 21:50:53][MemoryAccess::Player::PThread] Selected action: Create\n";
        assert_eq!(local_player_from_log(log), Some(1));
        assert_eq!(local_player_from_log("[INF][x] Assigned to player 77\n"), None);
        assert_eq!(local_player_from_log(""), None);
    }

    /// A fake BOTWM client speaking the server's wire format: every client message is padded to 7168 bytes,
    /// `[type][fields in declaration order]`, strings as length byte + UTF-8; server updates come back with a
    /// 2-byte length prefix and end with `TeleportData.Destination` (three f32).
    struct FakeClient(std::net::TcpStream);

    impl FakeClient {
        fn send(&mut self, body: &[u8]) {
            use std::io::Write;
            let mut msg = body.to_vec();
            msg.resize(7168, 0);
            self.0.write_all(&msg).unwrap();
        }

        fn connect(port: u16, name: &str) -> (Self, serde_json::Value) {
            use std::io::Read;
            let mut c = FakeClient(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
            c.0.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut body = vec![2u8, name.len() as u8];
            body.extend_from_slice(name.as_bytes());
            body.push(0); // empty password
            c.send(&body);
            let mut buf = vec![0u8; 4096];
            let n = c.0.read(&mut buf).unwrap();
            (c, serde_json::from_slice(&buf[..n]).unwrap())
        }

        /// Send an update at `pos` with `health`; return the teleport destination from the server's answer.
        fn update(&mut self, pos: [f32; 3], health: i32) -> [f32; 3] {
            use std::io::Read;
            let f = |v: f32| v.to_le_bytes();
            let mut b = vec![3u8];
            b.extend(f(720.0)); // WorldData.Time
            b.extend(0i32.to_le_bytes()); // Day
            b.extend(0i32.to_le_bytes()); // Weather
            pos.iter().for_each(|v| b.extend(f(*v)));
            for _ in 0..4 {
                [0.0, 0.0, 0.0, 1.0].iter().for_each(|v| b.extend(f(*v))); // Rotation1..4
            }
            b.extend(0i32.to_le_bytes()); // Animation
            b.extend(health.to_le_bytes());
            b.extend(f(0.0)); // AtkUp
            b.push(0); // IsEquipped
            b.push(0); // Equipment.WType
            b.extend([0u8; 12]); // Sword, Shield, Bow, Head, Upper, Lower
            b.extend([0u8; 2]); // Location
            b.extend([0u8; 48]); // Bomb, Bomb2, BombCube, BombCube2
            b.push(0); // EnemyData.Health: empty list
            b.push(0); // QuestData.Completed: empty list
            self.send(&b);
            let mut len = [0u8; 2];
            self.0.read_exact(&mut len).unwrap();
            let mut data = vec![0u8; u16::from_le_bytes(len) as usize];
            self.0.read_exact(&mut data).unwrap();
            let t = &data[data.len() - 12..];
            let g = |i: usize| f32::from_le_bytes(t[i..i + 4].try_into().unwrap());
            [g(0), g(4), g(8)]
        }

        fn disconnect(mut self) {
            self.send(&[4]);
        }
    }

    /// End-to-end against the real BOTWM 1.0.4 dedicated server (a scratch copy on port 5060, so a running
    /// session on 5050 is untouched): settings land in its ini, the console parser follows two fake players,
    /// `_Get` replies give positions and health, `Tp` reaches the teleported player's next update, Death Swap
    /// arms, and a clean disconnect is noticed by the next poll. Needs the .NET 6 runtime and the user's
    /// `%APPDATA%\BOTWM` files (the server creates them on its first run).
    /// `BOTWM_SERVER_DIR=<...\DedicatedServer> cargo test -p privcord-games --lib real_server -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_server_settings_party_and_teleport() {
        use std::io::{BufRead, Write};
        let src = std::path::PathBuf::from(std::env::var("BOTWM_SERVER_DIR").expect("BOTWM_SERVER_DIR"));
        let dir = std::env::temp_dir().join(format!("privcord-botwm-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            if e.path().is_file() {
                std::fs::copy(e.path(), dir.join(e.file_name())).unwrap();
            }
        }
        let settings = CoopSettings { mode: Mode::DeathSwap, swap_min: 2, swap_max: 3, ..Default::default() };
        let ini = std::fs::read_to_string(dir.join("ServerConfig.ini")).unwrap();
        let ini = apply_settings(&ini, &settings).replace("Port=5050", "Port=5060");
        std::fs::write(dir.join("ServerConfig.ini"), &ini).unwrap();
        println!("--- ServerConfig.ini ---\n{ini}\n---");

        let mut server = std::process::Command::new(dir.join("BOTW.DedicatedServer.exe"))
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("start the server");
        let mut stdin = server.stdin.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let stdout = server.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
                let _ = tx.send(String::from_utf8_lossy(&line).trim_end_matches('\r').to_owned());
            }
        });
        let party = std::cell::RefCell::new(Party::default());
        let mut events = Vec::new();
        let mut log = Vec::new();
        // Read console lines into the party until `done` holds or 10 s pass.
        let mut pump = |done: &dyn Fn(&Party, &[String]) -> bool| {
            let end = std::time::Instant::now() + Duration::from_secs(10);
            while !done(&party.borrow(), &log) {
                let left = end.saturating_duration_since(std::time::Instant::now());
                assert!(!left.is_zero(), "timed out; console so far:\n{}", log.join("\n"));
                if let Ok(line) = rx.recv_timeout(left) {
                    println!("{line}");
                    let (ev, _) = party.borrow_mut().feed(&line);
                    events.extend(ev);
                    log.push(line);
                }
            }
        };
        let mut console = |cmd: &str| {
            writeln!(stdin, "{cmd}").unwrap();
            stdin.flush().unwrap();
        };
        pump(&|_, log| log.iter().any(|l| l.contains("Type help to see available commands")));
        for cmd in settings.startup_commands() {
            console(&cmd);
        }
        pump(&|_, log| log.iter().any(|l| l.contains("Enabled death swap.")));

        let (mut a, ra) = FakeClient::connect(5060, "Alpha");
        let (mut b, rb) = FakeClient::connect(5060, "Bravo Two");
        assert_eq!((ra["Response"].as_i64(), ra["PlayerNumber"].as_i64()), (Some(1), Some(0)), "{ra}");
        assert_eq!(rb["PlayerNumber"].as_i64(), Some(1));
        assert_eq!(rb["Settings"]["GameMode"].as_i64(), Some(2), "Special=2 reached the server: {rb}");
        assert_eq!(rb["Settings"]["EnemySync"].as_bool(), Some(true));
        assert_eq!(rb["Settings"]["QuestSyncSettings"]["Shrines"].as_bool(), Some(true));
        assert_eq!(rb["Settings"]["QuestSyncSettings"]["Vanilla"].as_bool(), Some(false));
        pump(&|p, _| p.numbers() == vec![0, 1]);

        assert_eq!(a.update([100.0, 50.5, 200.0], 52), [0.0; 3]);
        assert_eq!(b.update([130.0, 50.0, 240.0], 12), [0.0; 3]);
        for n in [0, 1] {
            console(&get_command(n, Field::Position));
            console(&get_command(n, Field::Health));
        }
        pump(&|p, _| p.members().iter().all(|m| m.position.is_some() && m.health.is_some()));
        let m = party.borrow().members();
        assert_eq!(m[0].position, Some([100.0, 50.5, 200.0]));
        assert_eq!((m[0].health, m[1].health), (Some(52), Some(12)));
        assert_eq!(distance(m[0].position.unwrap(), m[1].position.unwrap()), 50.0);

        // Bravo Two → Alpha.
        console(&check_teleport(&party.borrow(), 1, 0).unwrap());
        pump(&|_, log| log.iter().any(|l| l.contains("Requested teleport of 1 players")));
        assert_eq!(b.update([130.0, 50.0, 240.0], 12), [100.0, 50.5, 200.0], "teleport delivered on the next update");
        assert_eq!(b.update([100.0, 50.5, 200.0], 12), [0.0; 3], "and only once");
        assert_eq!(a.update([100.0, 50.5, 200.0], 52), [0.0; 3], "nobody else moves");

        b.disconnect();
        std::thread::sleep(Duration::from_millis(300));
        console(&get_command(1, Field::Position));
        pump(&|p, _| p.numbers() == vec![0]);
        assert!(events.contains(&Event::Left { number: 1, name: "Bravo Two".into() }), "{events:?}");
        drop(a);
        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host that streams an endless line is cut off at MAX_REPLY instead of filling the friend's memory.
    #[tokio::test]
    async fn oversized_feed_reply_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let flood = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let chunk = vec![b'a'; 16 * 1024];
            while s.write_all(&chunk).await.is_ok() {}
        });
        let err = ask_feed(addr, &FeedRequest::Party).await.unwrap_err();
        assert!(err.to_string().contains("too long"), "{err:#}");
        flood.abort();
    }

    #[tokio::test]
    async fn feed_serves_the_party_and_teleports() {
        let party = Arc::new(Mutex::new(Party::default()));
        {
            let mut p = party.lock().await;
            p.feed("[10:00:00] Player A joined the server. Assigned to player 1.");
            p.feed("[10:00:00] Player B joined the server. Assigned to player 2.");
        }
        let (tx, mut rx) = mpsc::channel(4);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(serve_party(listener, party.clone(), CoopSettings::default(), tx));
        let reply = ask_feed(addr, &FeedRequest::Party).await.unwrap();
        let view = reply.party.unwrap();
        assert_eq!(view.members.len(), 2);
        assert_eq!(view.settings, CoopSettings::default());
        assert!(ask_feed(addr, &FeedRequest::Teleport { from: 1, to: 0 }).await.unwrap().ok);
        assert_eq!(rx.recv().await.unwrap(), "Tp p2 p1");
        let bad = ask_feed(addr, &FeedRequest::Teleport { from: 1, to: 5 }).await.unwrap();
        assert!(!bad.ok && bad.error.is_some());
        let mut raw = TcpStream::connect(addr).await.unwrap();
        raw.write_all(b"{\"op\":\"format c:\"}\n").await.unwrap();
        let mut reply = String::new();
        BufReader::new(raw).read_line(&mut reply).await.unwrap();
        assert!(reply.contains("bad request"));
        task.abort();
    }
}

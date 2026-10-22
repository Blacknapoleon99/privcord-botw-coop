//! Shared enemies (BotW multiplayer, phase 1) from a terminal, for testing without the PrivCord app.
//!
//! cargo run -p privcord-games --example botw_enemies_probe -- --me NAME [options]
//!   --me <name>          this player's name (must differ between the PCs)
//!   --relay <ip:port>    join this enemy relay
//!   --listen <ip:port>   host the enemy relay here (e.g. 127.0.0.1:5054, or 0.0.0.0:5054 for a LAN test) and join it
//!   --cemu <folder>      Cemu 1.26.2 folder (default: PrivCord's BotW multiplayer install)
//!   --secs <n>           stop after n seconds (default: run until Ctrl+C)
//!   --teleport x,y,z     test helper: first move Link there (his Havok motion state, like the mod does)
//!   --only               with --teleport: just teleport, don't start the sidecar
//!   --peek               print the patch's puppet table (count, AI skips, entries with their lease) and exit
//!   --enemy <hash hex>   print that enemy's life and current AS in this game and exit
//!   --heal               test helper: keep Link at 3+ hearts while the probe runs (fights near a camp)
//! Prints ownership changes and, every 2 s, the counts and the puppets' distance from the owner's enemy.

use std::time::Duration;

use privcord_botw_coop::botw_enemies::{self as rules};
use privcord_botw_coop::botw_enemies_sidecar::{self as sidecar, Event};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let me = arg("--me").expect("--me <name>");
    let cemu = arg("--cemu").map(Into::into).unwrap_or_else(|| {
        std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join(r"com.privcord.app\native-games\botwmp\app\cemu_1.26.2")
    });
    let relay: std::net::SocketAddr = match (arg("--listen"), arg("--relay")) {
        (Some(l), _) => {
            let listener = tokio::net::TcpListener::bind(&l).await.expect("bind relay");
            println!("relay listening on {l}");
            tokio::spawn(rules::run_relay(listener));
            let mut a: std::net::SocketAddr = l.parse().expect("--listen ip:port");
            if a.ip().is_unspecified() {
                a.set_ip([127, 0, 0, 1].into());
            }
            a
        }
        (None, Some(r)) => r.parse().expect("--relay ip:port"),
        (None, None) => ([127, 0, 0, 1], rules::RELAY_PORT).into(),
    };
    let secs: Option<u64> = arg("--secs").and_then(|s| s.parse().ok());
    if let Some(h) = arg("--enemy") {
        helpers::enemy(&cemu, u32::from_str_radix(&h, 16).expect("--enemy <hash hex>"));
        return;
    }
    if args.iter().any(|a| a == "--peek") {
        helpers::peek(&cemu);
        return;
    }
    if let Some(t) = arg("--teleport") {
        let p: Vec<f32> = t.split(',').map(|v| v.trim().parse().expect("--teleport x,y,z")).collect();
        match helpers::teleport(&cemu, [p[0], p[1], p[2]]) {
            Some(was) => println!("teleported Link from {was:?} to {p:?}"),
            None => println!("teleport failed (Cemu/patch/Link not found)"),
        }
        if args.iter().any(|a| a == "--only") {
            return;
        }
    }
    if args.iter().any(|a| a == "--heal") {
        let c = cemu.clone();
        std::thread::spawn(move || loop {
            helpers::heal(&c, 12);
            std::thread::sleep(Duration::from_millis(100));
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let s = sidecar::start(cemu, me.clone(), relay, tx);
    println!("{me}: enemy sidecar started, relay {relay}");
    let t0 = std::time::Instant::now();
    let deadline = async {
        match secs {
            Some(n) => tokio::time::sleep(Duration::from_secs(n)).await,
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    };
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            e = rx.recv() => match e {
                Some(Event::Attached { cave }) => println!("{:7.1}s attached, puppet table at {cave:#x}", t0.elapsed().as_secs_f32()),
                Some(Event::Notice(n)) => println!("{:7.1}s {n:?}", t0.elapsed().as_secs_f32()),
                Some(Event::Stats(st)) => {
                    let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
                    for e in &st.shared {
                        println!("         {wall} {:08x} {} ({:.2}, {:.2}, {:.2}) {}", e.hash, if e.owner { "owner " } else { "puppet" }, e.pos[0], e.pos[1], e.pos[2], e.anim);
                    }
                    println!(
                    "{:7.1}s link {:?} enemies {} owned {} puppets {} peers {} frames in/out {}/{} skips {} | sync err p50 {:.2} p95 {:.2} max {:.2} m, max step {:.2} m | AS asked/played {}/{} lag p50 {:.0} max {:.0} ms",
                    t0.elapsed().as_secs_f32(), st.link.map(|p| p.map(|v| v.round())), st.enemies, st.owned, st.puppets, st.peers, st.frames_in, st.frames_out, st.skips,
                    st.sync_err_p50, st.sync_err_p95, st.sync_err_max, st.max_step, st.anim_requests, st.anim_plays, st.anim_lag_p50, st.anim_lag_max
                    )
                }
                None => break,
            },
            _ = &mut deadline => break,
        }
    }
    s.stop();
    // Give the thread a moment to clear the puppet table.
    tokio::time::sleep(Duration::from_millis(300)).await;
    println!("stopped");
}

mod helpers {
    use std::path::Path;

    use privcord_botw_coop::botw_downed::{self as downed};
    use privcord_botw_coop::botw_downed_sidecar::mem::Cemu;

    fn open(cemu: &Path) -> Option<(Cemu, u32)> {
        let log = std::fs::read_to_string(cemu.join("log.txt")).ok()?;
        let (base, cave) = downed::parse_cemu_log(&log)?;
        let c = Cemu::open(&cemu.join("Cemu.exe"), base)?;
        let link = c.read_u32(cave + 0x20).filter(|&a| a != 0)?;
        Some((c, link))
    }

    fn f3(c: &Cemu, a: u32) -> Option<[f32; 3]> {
        Some([c.read_u32(a)?, c.read_u32(a + 4)?, c.read_u32(a + 8)?].map(f32::from_bits))
    }

    /// Move Link like the BOTWM DLL does: his Havok motion state (translation and both centres of mass).
    pub fn teleport(cemu: &Path, p: [f32; 3]) -> Option<[f32; 3]> {
        let (c, link) = open(cemu)?;
        let mut ms = link;
        for o in [0x3a0, 0x50, 4, 0x80, 0, 0x5c, 0x18] {
            ms = c.read_u32(ms + o).filter(|&v| v != 0)?;
        }
        let was = f3(&c, ms + 0x30)?;
        let b: Vec<u8> = p.iter().flat_map(|v| v.to_be_bytes()).collect();
        for o in [0x30, 0x40, 0x50] {
            c.write(ms + o, &b);
        }
        Some(was)
    }

    /// Print the puppet table twice, a second apart (are the AI skips still rising?).
    pub fn peek(cemu: &Path) {
        use privcord_botw_coop::botw_enemies as rules;
        let Some(log) = std::fs::read_to_string(cemu.join("log.txt")).ok() else { return println!("no log.txt") };
        let Some((base, cave)) = rules::parse_cemu_log(&log, rules::PATCH_GROUP) else { return println!("patch not loaded") };
        let Some(c) = Cemu::open(&cemu.join("Cemu.exe"), base) else { return println!("Cemu not running") };
        for _ in 0..2 {
            let n = c.read_u32(cave + rules::OFF_COUNT).unwrap_or(0);
            let skips = c.read_u32(cave + rules::OFF_SKIPS).unwrap_or(0);
            let entries: Vec<String> = (0..n.min(64))
                .map(|i| {
                    let a = cave + rules::OFF_TABLE + i * rules::ENTRY_LEN;
                    let name = c.read(a + rules::ENTRY_NAME, 24).map(|b| rules::Anim(b.try_into().unwrap()).as_str().to_owned()).unwrap_or_default();
                    format!(
                        "{:08x}/lease {}/AS {} ({}/{})",
                        c.read_u32(a).unwrap_or(0),
                        c.read_u32(a + 4).unwrap_or(0),
                        name,
                        c.read_u32(a + rules::ENTRY_REQ).unwrap_or(0),
                        c.read_u32(a + rules::ENTRY_DONE).unwrap_or(0)
                    )
                })
                .collect();
            let plays = c.read_u32(cave + rules::OFF_PLAYS).unwrap_or(0);
            println!("count {n} skips {skips} AS plays {plays} {entries:?}");
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// Life and current AS of the enemy with HashId `hash` (any state, dead placeholders too).
    pub fn enemy(cemu: &Path, hash: u32) {
        let Some((c, link)) = open(cemu) else { return println!("Cemu/patch/Link not found") };
        let list = c.read_u32(link + 0x4f0).unwrap_or(0);
        let off = c.read_u32(list + 0x24).unwrap_or(0);
        let head = list + 0x18;
        let mut n = c.read_u32(head).unwrap_or(0);
        for _ in 0..5000 {
            if n == 0 || n == head {
                break;
            }
            let a = n.wrapping_sub(off);
            n = c.read_u32(n).unwrap_or(0);
            if c.read_u32(a + 0x408) != Some(hash) {
                continue;
            }
            let anim = (|| {
                let p = c.read_u32(c.read_u32(c.read_u32(c.read_u32(a + 0x394)? + 0x98)? + 0x1c)?)?;
                let b = c.read(c.read_u32(p + 4)?, 24)?;
                Some(String::from_utf8_lossy(&b[..b.iter().position(|&x| x == 0).unwrap_or(24)]).into_owned())
            })();
            let pos: Vec<f32> = [0x204, 0x214, 0x224].iter().map(|o| f32::from_bits(c.read_u32(a + o).unwrap_or(0))).collect();
            println!("{hash:08x} actor {a:08x} life {} AS {:?} pos {pos:?}", c.read_u32(a + 0x540).unwrap_or(0) as i32, anim);
        }
    }

    /// Keep Link's life (quarter hearts, actor +0x540) at least `min`.
    pub fn heal(cemu: &Path, min: i32) {
        if let Some((c, link)) = open(cemu) {
            if let Some(l) = c.read_u32(link + 0x540).map(|v| v as i32) {
                if l > 0 && l < min {
                    c.write_u32(link + 0x540, min as u32);
                }
            }
        }
    }
}

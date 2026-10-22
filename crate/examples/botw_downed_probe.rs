//! Run the BotW downed/revive sidecar from a terminal, without the PrivCord app (for in-game tests).
//!
//! ```text
//! cargo run -p privcord-games --example botw_downed_probe -- [options]
//!   --cemu <dir>        Cemu 1.26.2 folder (default: PrivCord's install under %LOCALAPPDATA%)
//!   --name <name>       this player's name (default: the Windows user name)
//!   --relay <ip:port>   join this relay instead of hosting one on 127.0.0.1:5053
//!   --listen <ip:port>  host the relay on this address (e.g. 0.0.0.0:5053 for a LAN test with another PC)
//!   --fake-friend       add a pretend friend who is standing elsewhere, and who comes over and revives you
//!                       (holds the key for the full 12 s) whenever you go down
//! ```
//! Without a friend (real or fake) a downed player is alone, so everyone is down: game over after 3 s.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use privcord_botw_coop::botw_downed::{self as rules, Msg};
use privcord_botw_coop::botw_downed_sidecar::{self as sidecar, Event};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "probe_support/bar.rs"]
mod probe_bar;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut cemu = std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join(r"com.privcord.app\native-games\botwmp\app\cemu_1.26.2"))
        .unwrap_or_default();
    let mut name = std::env::var("USERNAME").unwrap_or_else(|_| "probe".into());
    let mut relay: Option<SocketAddr> = None;
    let mut listen: SocketAddr = SocketAddr::from(([127, 0, 0, 1], rules::RELAY_PORT));
    let mut fake = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cemu" => cemu = args.next().expect("--cemu <dir>").into(),
            "--name" => name = args.next().expect("--name <name>"),
            "--relay" => relay = Some(args.next().expect("--relay <ip:port>").parse()?),
            "--listen" => listen = args.next().expect("--listen <ip:port>").parse()?,
            "--fake-friend" => fake = true,
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    let relay = match relay {
        Some(r) => r,
        None => {
            let l = tokio::net::TcpListener::bind(listen).await?;
            println!("relay listening on {listen}");
            tokio::spawn(rules::run_relay(l));
            SocketAddr::from(([127, 0, 0, 1], listen.port()))
        }
    };
    if fake {
        tokio::spawn(fake_friend(relay, name.clone()));
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let side = sidecar::start(cemu.clone(), name.clone(), relay, tx);
    println!("watching {} as {name:?}, relay {relay}. Ctrl+C to stop (disarms the patch).", cemu.display());
    let mut last_tick = 0u8;
    let bar = probe_bar::start();
    loop {
        tokio::select! {
            e = rx.recv() => match e {
                Some(Event::Attached { cave }) => println!("attached to Cemu (block at {cave:#010x}), patch armed"),
                Some(Event::Notice(n)) => {
                    println!("NOTICE  {n:?}");
                    match n {
                        // Audible feedback, since the probe has no overlay: rising triple = revive done.
                        rules::Notice::RevivedFriend { .. } | rules::Notice::Revived { .. } => beep(&[(660, 120), (880, 120), (1100, 200)]),
                        rules::Notice::Downed | rules::Notice::FriendDowned { .. } => beep(&[(330, 300)]),
                        _ => {}
                    }
                }
                Some(Event::Overlay(o)) => {
                    println!("overlay {o:?}");
                    *bar.lock().unwrap() = probe_bar::describe(&o);
                    // One tick per 10 % while reviving.
                    if let Some((_, pct)) = o.prompt.as_ref().or(o.revived_by.as_ref()) {
                        if *pct > 0 && *pct / 10 != last_tick {
                            beep(&[(1000, 60)]);
                        }
                        last_tick = *pct / 10;
                    } else {
                        last_tick = 0;
                    }
                }
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    side.stop();
    tokio::time::sleep(Duration::from_millis(300)).await; // let it disarm
    Ok(())
}

/// A pretend second player: stands 50 m away while `me` is up; when `me` goes down it walks over (same position) and
/// holds the revive key for the full time.
async fn fake_friend(relay: SocketAddr, me: String) {
    let friend = "FakeFriend".to_string();
    loop {
        let Ok(sock) = tokio::net::TcpStream::connect(relay).await else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        let (mut down, mut pos) = (false, None::<[f32; 3]>);
        let mut holding: Option<tokio::time::Instant> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                l = lines.next_line() => match l {
                    Ok(Some(l)) => {
                        if let Ok(Msg::Status { name, downed, pos: p }) = serde_json::from_str::<Msg>(&l) {
                            if name == me {
                                if downed && !down {
                                    println!("FakeFriend: {me} is down, walking over…");
                                    holding = Some(tokio::time::Instant::now() + Duration::from_secs(3));
                                }
                                down = downed;
                                pos = p;
                            }
                        }
                    }
                    _ => break,
                },
                _ = tick.tick() => {
                    let mut out = Vec::new();
                    let here = pos.map(|p| if down { p } else { [p[0] + 50.0, p[1], p[2]] });
                    out.push(Msg::Status { name: friend.clone(), downed: false, pos: here });
                    if let (true, Some(start)) = (down, holding) {
                        let now = tokio::time::Instant::now();
                        if now >= start {
                            let held = now - start;
                            if held >= rules::REVIVE_HOLD {
                                out.push(Msg::Revive { by: friend.clone(), target: me.clone() });
                                println!("FakeFriend: revive sent");
                                holding = None;
                            } else {
                                let pct = (held.as_millis() * 100 / rules::REVIVE_HOLD.as_millis()) as u8;
                                out.push(Msg::Reviving { by: friend.clone(), target: me.clone(), percent: pct });
                            }
                        }
                    }
                    for m in out {
                        let s = serde_json::to_string(&m).unwrap();
                        if w.write_all(format!("{s}\n").as_bytes()).await.is_err() { break; }
                    }
                }
            }
        }
    }
}

/// Play (frequency Hz, milliseconds) tones on a background thread.
fn beep(tones: &[(u32, u32)]) {
    let tones = tones.to_vec();
    std::thread::spawn(move || {
        for (f, ms) in tones {
            #[cfg(windows)]
            unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::Beep(f, ms);
            }
            #[cfg(not(windows))]
            let _ = (f, ms);
        }
    });
}

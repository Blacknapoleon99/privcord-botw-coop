//! Run the production smoother (`botw_smooth_sidecar`) against the local Cemu 1.26.2 for N seconds:
//! `cargo run -p privcord-games --release --example botw_smooth_live -- <seconds>`
fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let exe = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap())
        .join("com.privcord.app/native-games/botwmp/app/cemu_1.26.2/Cemu.exe");
    tracing_subscriber_lite();
    let s = privcord_botw_coop::botw_smooth_sidecar::start(exe);
    std::thread::sleep(std::time::Duration::from_secs(secs));
    s.stop();
    std::thread::sleep(std::time::Duration::from_millis(100));
    println!("stopped");
}

fn tracing_subscriber_lite() {}

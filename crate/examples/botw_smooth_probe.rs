//! Live test of `botw_smooth` on a running BotW multiplayer game (Windows, Cemu 1.26.2 + BOTWM 1.0.4 DLL).
//!
//! ```text
//! cargo run -p privcord-games --example botw_smooth_probe -- <player number> <seconds>
//! ```
//! Reads the position the mod's DLL received for that remote player (`Player+0x90`, host memory, via the DLL's player
//! table `std::map<int, Player*>` at InjectDLL+0x52ff0) about 2000 times a second, smooths it with a jitter buffer, and
//! writes the result into the three guest Vec3s the DLL writes the puppet's physics motion state through
//! (`Player+0x08/+0x38/+0x40` Vec3Watched -> component objects -> host pointer into guest memory at +0x8), right after
//! each DLL write. Nothing is injected; it is ReadProcessMemory/WriteProcessMemory, like the downed sidecar.
//! Offsets are for InjectDLL.dll 1.0.4 (372736 bytes); any other size is refused.

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use privcord_botw_coop::botw_smooth::{Smoother, TELEPORT_M};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            Diagnostics::{
                Debug::{ReadProcessMemory, WriteProcessMemory},
                ToolHelp::{
                    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W, Process32FirstW,
                    Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPPROCESS,
                },
            },
            Threading::{OpenProcess, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE},
        },
    };

    let mut args = std::env::args().skip(1);
    let who: i32 = args.next().unwrap_or_else(|| "2".into()).parse()?;
    let seconds: u64 = args.next().unwrap_or_else(|| "20".into()).parse()?;

    let wide = |w: &[u16]| String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())]);
    // Cemu 1.26.2's pid.
    let pid = unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            if wide(&e.szExeFile).eq_ignore_ascii_case("Cemu.exe") {
                found = Some(e.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        found.ok_or_else(|| anyhow::anyhow!("Cemu is not running"))?
    };
    // InjectDLL.dll's base and size.
    let (dll, dll_size) = unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid);
        let mut m = MODULEENTRY32W { dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut ok = Module32FirstW(snap, &mut m) != 0;
        while ok {
            if wide(&m.szModule).eq_ignore_ascii_case("InjectDLL.dll") {
                found = Some((m.modBaseAddr as u64, m.modBaseSize));
            }
            ok = Module32NextW(snap, &mut m) != 0;
        }
        CloseHandle(snap);
        found.ok_or_else(|| anyhow::anyhow!("the BOTWM DLL is not loaded"))?
    };
    anyhow::ensure!(dll_size == 372_736, "unexpected InjectDLL.dll size {dll_size} (offsets are for BOTWM 1.0.4)");
    let h = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION, 0, pid) };
    anyhow::ensure!(!h.is_null(), "OpenProcess failed");
    let read = |a: u64, n: usize| -> Option<Vec<u8>> {
        let mut b = vec![0u8; n];
        let mut got = 0usize;
        (unsafe { ReadProcessMemory(h, a as *const _, b.as_mut_ptr().cast(), n, &mut got) } != 0 && got == n).then_some(b)
    };
    let r64 = |a: u64| read(a, 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
    // kernel32's WriteProcessMemory changes and restores page protection around every write; inside Cemu's single
    // 3.3 GB guest-memory region that costs ~12 ms per call (measured). ntdll's NtWriteVirtualMemory writes the
    // already read-write pages directly.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtWriteVirtualMemory(h: *mut core::ffi::c_void, addr: *mut core::ffi::c_void, buf: *const core::ffi::c_void, n: usize, written: *mut usize) -> i32;
    }
    let _ = WriteProcessMemory; // kept for reference
    let write_be3 = |a: u64, p: [f32; 3]| {
        let mut b = [0u8; 12];
        for (i, v) in p.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        let mut put = 0usize;
        unsafe { NtWriteVirtualMemory(h, a as *mut _, b.as_ptr().cast(), 12, &mut put) >= 0 }
    };
    // The DLL's player table: MSVC std::map<int, Player*> (head at +0, nodes: left +0, parent +8, right +0x10,
    // is-nil +0x19, key +0x20, value +0x28).
    let find_player = || -> Option<u64> {
        let head = r64(dll + 0x52ff0)?;
        let mut stack = vec![r64(head + 8)?];
        while let Some(n) = stack.pop() {
            if n == 0 || n == head {
                continue;
            }
            let node = read(n, 0x30)?;
            if node[0x19] != 0 {
                continue;
            }
            let key = i32::from_le_bytes(node[0x20..0x24].try_into().unwrap());
            if key == who {
                return Some(u64::from_le_bytes(node[0x28..0x30].try_into().unwrap()));
            }
            stack.push(u64::from_le_bytes(node[0..8].try_into().unwrap()));
            stack.push(u64::from_le_bytes(node[0x10..0x18].try_into().unwrap()));
        }
        None
    };
    // The three host addresses (into guest memory) of the x components the DLL writes: the y and z floats follow.
    let targets = |player: u64| -> Option<Vec<u64>> {
        let mut out = Vec::new();
        for off in [0x8u64, 0x38, 0x40] {
            let v = r64(player + off)?;
            let comp_x = r64(v + 0x10)?;
            out.push(r64(comp_x + 0x8)?);
        }
        Some(out)
    };
    let write_raw = |a: u64, b: &[u8]| {
        let mut put = 0usize;
        unsafe { NtWriteVirtualMemory(h, a as *mut _, b.as_ptr().cast(), b.len(), &mut put) >= 0 }
    };
    let read_le3 = |a: u64| read(a, 12).map(|b| [0, 4, 8].map(|i| f32::from_le_bytes(b[i..i + 4].try_into().unwrap())));
    let read_be3 = |a: u64| read(a, 12).map(|b| [0, 4, 8].map(|i| f32::from_be_bytes(b[i..i + 4].try_into().unwrap())));

    let start = Instant::now();
    let now_ms = || start.elapsed().as_secs_f64() * 1000.0;
    let mut s = Smoother::new();
    let (mut player, mut addrs) = (None, Vec::new());
    let mut last_resolve = Instant::now() - Duration::from_secs(1);
    let mut last_in: Option<[f32; 3]> = None;
    let mut last_out: Option<[f32; 3]> = None;
    let (mut writes, mut updates, mut overwritten, mut loops) = (0u64, 0u64, 0u64, 0u64);
    let (mut out_changes, mut not_near, mut no_sample, mut no_addrs) = (0u64, 0u64, 0u64, 0u64);
    let mut prev_out: Option<[f32; 3]> = None;
    println!("smoothing player {who} for {seconds} s (Cemu pid {pid}, DLL at {dll:#x})");
    while start.elapsed() < Duration::from_secs(seconds) {
        if last_resolve.elapsed() > Duration::from_millis(200) {
            last_resolve = Instant::now();
            player = find_player();
            addrs = player.and_then(targets).unwrap_or_default();
        }
        let Some(pl) = player else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if let Some(p) = read_le3(pl + 0x90) {
            if Some(p) != last_in {
                if let Some(l) = last_in {
                    let d = ((p[0] - l[0]).powi(2) + (p[1] - l[1]).powi(2) + (p[2] - l[2]).powi(2)).sqrt();
                    if updates < 12 {
                        println!("  update {updates}: {:.1} ms, moved {d:.3} m -> {p:?}", now_ms());
                    }
                }
                s.push(now_ms(), p);
                last_in = Some(p);
                updates += 1;
            }
        }
        let tr = Instant::now();
        let shown_probe = addrs.first().and_then(|&a| read_be3(a));
        if writes < 5 && shown_probe.is_some() && loops % 200 == 0 {
            println!("    1 guest read took {:?}", tr.elapsed());
        }
        let t_now = now_ms();
        let sampled = s.sample(t_now);
        if sampled.is_none() {
            no_sample += 1;
        }
        if sampled != prev_out {
            out_changes += 1;
            prev_out = sampled;
        }
        if addrs.is_empty() {
            no_addrs += 1;
        }
        if let (Some(out), Some(target), false) = (sampled, last_in, addrs.is_empty()) {
            let near = ((out[0] - target[0]).powi(2) + (out[1] - target[1]).powi(2) + (out[2] - target[2]).powi(2)).sqrt() < TELEPORT_M;
            // Rewrite when our value moved, or when the DLL overwrote it since our last write.
            let shown = read_be3(addrs[0]);
            let dll_wrote = shown.is_some() && shown != last_out;
            if !near {
                not_near += 1;
            }
            if near && (Some(out) != last_out || dll_wrote) {
                if dll_wrote {
                    overwritten += 1;
                }
                let tw = Instant::now();
                for &a in &addrs {
                    write_be3(a, out);
                }
                // Feed the DLL's own source too, so its next 16 ms write agrees with ours instead of racing it with the
                // raw (newer) position: the Player+0x08 Vec3Watched host copy (x, y, z LE floats at +0) = smoothed,
                // and its velocity (+0x84) = 0, so it writes exactly that.
                if let Some(v8) = r64(pl + 0x8) {
                    let mut b = [0u8; 12];
                    for (i, c) in out.iter().enumerate() {
                        b[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
                    }
                    write_raw(v8, &b);
                    write_raw(pl + 0x84, &[0u8; 12]);
                }
                let tw = tw.elapsed();
                if writes < 5 {
                    println!("    3 writes took {:?}", tw);
                }
                last_out = Some(out);
                writes += 1;
            }
        }
        // thread::sleep can't wait less than the ~15 ms Windows timer tick; spin (yielding) for ~0.4 ms instead.
        let until = Instant::now() + Duration::from_micros(400);
        while Instant::now() < until {
            std::thread::yield_now();
        }
        loops += 1;
    }
    unsafe { CloseHandle(h) };
    println!("done: {loops} loops, {updates} position updates in, {writes} smoothed writes, {overwritten} DLL writes replaced");
    println!("      output changes {out_changes}, not near {not_near}, no sample {no_sample}, no addrs {no_addrs}, smoother resets {}", s.resets);
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}

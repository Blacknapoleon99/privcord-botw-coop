//! Runs [`crate::botw_smooth`] on the live game: smooths every remote player's puppet in BotW multiplayer.
//!
//! How (all measured/verified on the main PC, 2026-10-20; offsets for BOTWM 1.0.4's InjectDLL.dll, 372736 bytes):
//! - The DLL keeps `std::map<int, Player*>` at `InjectDLL+0x52ff0` (MSVC: head at +0; nodes: left +0, parent +8, right
//!   +0x10, is-nil +0x19, key +0x20, value +0x28). A Player with a puppet has its actor (guest address) at `+0`.
//! - `Player+0x90`: the last position the server sent (3 LE floats), written when an update arrives.
//! - The DLL's 16 ms loop writes `Player+0x08 position + t * velocity(Player+0x84)` through three `Vec3Watched`
//!   (`Player+0x08/+0x38/+0x40`) into the puppet's physics motion state; each one's x component object (`+0x10`) holds
//!   at `+0x8` a host pointer into guest memory, where the three big-endian floats x, y, z are.
//! - This thread reads `+0x90` ~2000x/s, plays the positions back through a jitter buffer, and writes the result into
//!   those guest floats right after each DLL write, and into the DLL's own source (`Player+0x08` host copy, velocity 0),
//!   so its next write agrees. Result on a real walk: frames where the friend froze 14 % -> 0 %.
//! - Writes use `NtWriteVirtualMemory`: kernel32's `WriteProcessMemory` toggles page protection and costs ~12 ms per
//!   call inside Cemu's single 3.3 GB guest region (measured), far too slow.
//!
//! Nothing is injected. The thread waits on a high-resolution waitable timer (0.5 ms), not a spin loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::botw_smooth::{Smoother, TELEPORT_M};

/// InjectDLL.dll 1.0.4's size; any other build is left alone (the offsets would be wrong).
pub const DLL_SIZE: u32 = 372_736;
const PLAYER_MAP: u64 = 0x52ff0;

/// A running smoother thread. [`SmoothSidecar::stop`] ends it.
pub struct SmoothSidecar {
    stop: Arc<AtomicBool>,
}

impl SmoothSidecar {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Start smoothing the remote players of the Cemu at `cemu_exe`. Returns at once; the thread waits for Cemu and the
/// DLL by itself.
pub fn start(cemu_exe: PathBuf) -> SmoothSidecar {
    let stop = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("botw-smooth".into())
        .spawn(move || {
            #[cfg(windows)]
            win::run(&cemu_exe, &s);
            #[cfg(not(windows))]
            let _ = (cemu_exe, s);
        })
        .ok();
    SmoothSidecar { stop }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// One remote player being smoothed.
struct Track {
    smoother: Smoother,
    last_in: Option<[f32; 3]>,
    last_out: Option<[f32; 3]>,
    /// Host addresses of the three guest Vec3s, and of the DLL's source copy.
    guest: Vec<u64>,
    source: Option<u64>,
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE},
            },
            Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS,
                WaitForSingleObject,
            },
        },
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtWriteVirtualMemory(h: HANDLE, addr: *mut core::ffi::c_void, buf: *const core::ffi::c_void, n: usize, written: *mut usize) -> i32;
    }

    struct Proc {
        h: HANDLE,
        dll: u64,
    }

    impl Drop for Proc {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.h) };
        }
    }

    impl Proc {
        fn read(&self, a: u64, n: usize) -> Option<Vec<u8>> {
            let mut b = vec![0u8; n];
            let mut got = 0usize;
            (unsafe { ReadProcessMemory(self.h, a as *const _, b.as_mut_ptr().cast(), n, &mut got) } != 0 && got == n).then_some(b)
        }
        fn r64(&self, a: u64) -> Option<u64> {
            self.read(a, 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        }
        fn floats(&self, a: u64, big_endian: bool) -> Option<[f32; 3]> {
            let b = self.read(a, 12)?;
            Some([0, 4, 8].map(|i| {
                let w: [u8; 4] = b[i..i + 4].try_into().unwrap();
                if big_endian { f32::from_be_bytes(w) } else { f32::from_le_bytes(w) }
            }))
        }
        fn write(&self, a: u64, b: &[u8]) -> bool {
            let mut put = 0usize;
            unsafe { NtWriteVirtualMemory(self.h, a as *mut _, b.as_ptr().cast(), b.len(), &mut put) >= 0 }
        }
        fn write_floats(&self, a: u64, p: [f32; 3], big_endian: bool) -> bool {
            let mut b = [0u8; 12];
            for (i, v) in p.iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&if big_endian { v.to_be_bytes() } else { v.to_le_bytes() });
            }
            self.write(a, &b)
        }
        /// (player number, Player*) for every entry of the DLL's table.
        fn players(&self) -> Option<Vec<(i32, u64)>> {
            let head = self.r64(self.dll + PLAYER_MAP)?;
            let mut out = Vec::new();
            let mut stack = vec![self.r64(head + 8)?];
            while let Some(n) = stack.pop() {
                if n == 0 || n == head || out.len() > 64 {
                    continue;
                }
                let node = self.read(n, 0x30)?;
                if node[0x19] != 0 {
                    continue;
                }
                out.push((i32::from_le_bytes(node[0x20..0x24].try_into().unwrap()), u64::from_le_bytes(node[0x28..0x30].try_into().unwrap())));
                stack.push(u64::from_le_bytes(node[0..8].try_into().unwrap()));
                stack.push(u64::from_le_bytes(node[0x10..0x18].try_into().unwrap()));
            }
            Some(out)
        }
    }

    fn open(cemu_exe: &std::path::Path) -> Option<Proc> {
        // The same Cemu the downed sidecar uses.
        let probe = crate::botw_downed_sidecar::open_cemu(cemu_exe)?;
        let pid = probe.0;
        let (dll, size) = unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid);
            if snap.is_null() || snap as isize == -1 {
                return None;
            }
            let mut m = MODULEENTRY32W { dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
            let mut found = None;
            let mut ok = Module32FirstW(snap, &mut m) != 0;
            while ok {
                let name = String::from_utf16_lossy(&m.szModule[..m.szModule.iter().position(|&c| c == 0).unwrap_or(256)]);
                if name.eq_ignore_ascii_case("InjectDLL.dll") {
                    found = Some((m.modBaseAddr as u64, m.modBaseSize));
                }
                ok = Module32NextW(snap, &mut m) != 0;
            }
            CloseHandle(snap);
            found?
        };
        if size != DLL_SIZE {
            tracing::warn!(size, "smooth: unknown InjectDLL.dll build, not smoothing");
            return None;
        }
        Some(Proc { h: probe.1, dll })
    }

    pub fn run(cemu_exe: &std::path::Path, stop: &AtomicBool) {
        let timer = unsafe { CreateWaitableTimerExW(std::ptr::null(), std::ptr::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS) };
        let wait = |micros: i64| unsafe {
            if timer.is_null() {
                std::thread::sleep(Duration::from_micros(micros as u64));
            } else {
                let due = -(micros * 10); // relative, 100 ns units
                SetWaitableTimer(timer, &due, 0, None, std::ptr::null(), 0);
                WaitForSingleObject(timer, INFINITE);
            }
        };
        let start = Instant::now();
        let now_ms = || start.elapsed().as_secs_f64() * 1000.0;
        let mut proc: Option<Proc> = None;
        let mut tracks: HashMap<u64, Track> = HashMap::new();
        let mut last_scan = Instant::now() - Duration::from_secs(10);
        while !stop.load(Ordering::Relaxed) {
            if proc.is_none() {
                proc = open(cemu_exe);
                if proc.is_none() {
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                tracing::info!("smooth: attached to the BOTWM DLL");
            }
            let p = proc.as_ref().unwrap();
            // Re-read the player table (and each puppet's guest addresses) every 200 ms.
            if last_scan.elapsed() > Duration::from_millis(200) {
                last_scan = Instant::now();
                let Some(players) = p.players() else {
                    proc = None;
                    tracks.clear();
                    continue;
                };
                let live: Vec<u64> = players.iter().filter(|(_, pl)| p.r64(*pl).is_some_and(|actor| actor & 0xffff_ffff != 0)).map(|(_, pl)| *pl).collect();
                tracks.retain(|pl, _| live.contains(pl));
                for pl in live {
                    let guest: Option<Vec<u64>> = [0x8u64, 0x38, 0x40].iter().map(|off| p.r64(pl + off).and_then(|v| p.r64(v + 0x10)).and_then(|c| p.r64(c + 0x8))).collect();
                    let t = tracks.entry(pl).or_insert_with(|| Track { smoother: Smoother::new(), last_in: None, last_out: None, guest: Vec::new(), source: None });
                    t.guest = guest.unwrap_or_default();
                    t.source = p.r64(pl + 0x8);
                }
            }
            for (&pl, t) in tracks.iter_mut() {
                if let Some(pos) = p.floats(pl + 0x90, false) {
                    if Some(pos) != t.last_in {
                        t.smoother.push(now_ms(), pos);
                        t.last_in = Some(pos);
                    }
                }
                let (Some(out), Some(target)) = (t.smoother.sample(now_ms()), t.last_in) else { continue };
                if t.guest.len() != 3 || dist(out, target) > TELEPORT_M {
                    continue;
                }
                let shown = p.floats(t.guest[0], true);
                if Some(out) != t.last_out || (shown.is_some() && shown != t.last_out) {
                    for &a in &t.guest {
                        p.write_floats(a, out, true);
                    }
                    if let Some(src) = t.source {
                        p.write_floats(src, out, false);
                        p.write(pl + 0x84, &[0u8; 12]);
                    }
                    t.last_out = Some(out);
                }
            }
            wait(if tracks.is_empty() { 50_000 } else { 500 });
        }
        if !timer.is_null() {
            unsafe { CloseHandle(timer) };
        }
    }
}

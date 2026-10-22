//! The BotW downed/revive sidecar loop (see [`crate::botw_downed`] for the rules): finds Cemu 1.26.2, reads the patch's
//! codecave block plus Link's health and position about ten times a second with ReadProcessMemory, runs the rules,
//! writes the results back, and swaps status lines with the other players through the relay. Nothing is injected into
//! Cemu. The PrivCord app turns [`Event`]s into its overlay window and toasts; `examples/botw_downed_probe.rs` prints
//! them, for testing without the app.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::botw_downed::{self as rules, Block, Downed, Msg, Notice, Overlay, Write};

/// What the loop reports.
#[derive(Clone, Debug)]
pub enum Event {
    /// Found Cemu with the patch applied (guest address of the codecave block).
    Attached { cave: u32 },
    Notice(Notice),
    /// The overlay changed.
    Overlay(Overlay),
}

/// A running sidecar; [`Sidecar::stop`] ends it and disarms the patch (back to vanilla deaths).
pub struct Sidecar {
    stop: tokio::sync::watch::Sender<bool>,
}

impl Sidecar {
    pub fn stop(self) {
        let _ = self.stop.send(true);
    }
}

/// Start watching Cemu at `cemu_root` (the folder with `Cemu.exe` and `log.txt`) for player `me`, talking to the relay
/// at `relay` (PrivCord: 127.0.0.1:5053, the host's relay or the tunnel to it). Must be called inside a Tokio runtime.
pub fn start(cemu_root: PathBuf, me: String, relay: std::net::SocketAddr, events: mpsc::UnboundedSender<Event>) -> Sidecar {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Msg>();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Msg>();
    tokio::spawn(relay_client(relay, in_tx, out_rx, stop_rx.clone()));
    tokio::spawn(run(cemu_root, me, in_rx, out_tx, events, stop_rx));
    Sidecar { stop: stop_tx }
}

fn overlay_changed(events: &mpsc::UnboundedSender<Event>, shown: &mut Overlay, o: Overlay) {
    if *shown != o {
        let _ = events.send(Event::Overlay(o.clone()));
        *shown = o;
    }
}

async fn relay_client(relay: std::net::SocketAddr, in_tx: mpsc::UnboundedSender<Msg>, mut out_rx: mpsc::UnboundedReceiver<Msg>, mut stop: tokio::sync::watch::Receiver<bool>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    while !*stop.borrow() {
        let sock = match tokio::net::TcpStream::connect(relay).await {
            Ok(s) => s,
            Err(_) => {
                // Not up yet (the tunnel or the host is still starting): drop what queued up and retry.
                while out_rx.try_recv().is_ok() {}
                tokio::select! { _ = tokio::time::sleep(Duration::from_secs(2)) => continue, _ = stop.changed() => return }
            }
        };
        let (r, mut w) = sock.into_split();
        let mut lines = BufReader::new(r).lines();
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(l)) => if let Ok(m) = serde_json::from_str::<Msg>(&l) { let _ = in_tx.send(m); },
                    _ => break,
                },
                m = out_rx.recv() => match m {
                    Some(m) => {
                        let Ok(s) = serde_json::to_string(&m) else { continue };
                        if w.write_all(format!("{s}\n").as_bytes()).await.is_err() { break; }
                    }
                    None => return,
                },
                _ = stop.changed() => return,
            }
        }
    }
}

async fn run(cemu_root: PathBuf, me: String, mut in_rx: mpsc::UnboundedReceiver<Msg>, out_tx: mpsc::UnboundedSender<Msg>, events: mpsc::UnboundedSender<Event>, stop: tokio::sync::watch::Receiver<bool>) {
    let mut ctl = Downed::new(me);
    let mut mem: Option<(mem::Cemu, u32)> = None;
    let mut last_attach = Instant::now() - Duration::from_secs(10);
    let mut last_status = Instant::now() - Duration::from_secs(10);
    let mut shown = Overlay::default();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    while !*stop.borrow() {
        tick.tick().await;
        let now = Instant::now();
        while let Ok(m) = in_rx.try_recv() {
            ctl.receive(now, m);
        }
        if mem.as_ref().is_some_and(|(c, _)| !c.alive()) {
            mem = None;
        }
        if mem.is_none() && now.duration_since(last_attach) >= Duration::from_secs(1) {
            last_attach = now;
            mem = attach(&cemu_root);
            if let Some((_, cave)) = &mem {
                let _ = events.send(Event::Attached { cave: *cave });
            }
        }
        let snapshot = mem.as_ref().and_then(|(c, cave)| {
            let block = Block::parse(&c.read(*cave, rules::BLOCK_LEN)?)?;
            let life = (block.life_ptr != 0).then(|| c.read_u32(block.life_ptr).map(|v| v as i32)).flatten();
            let pos = (block.self_actor != 0)
                .then(|| {
                    let p = rules::ACTOR_POS.map(|o| c.read_u32(block.self_actor + o).map(f32::from_bits));
                    Some([p[0]?, p[1]?, p[2]?])
                })
                .flatten();
            Some((block, life, pos, c.is_foreground()))
        });
        let Some((block, life, pos, focused)) = snapshot else {
            overlay_changed(&events, &mut shown, Overlay::default());
            continue;
        };
        let key = focused && mem::key_down(rules::REVIVE_KEY_VK);
        if let Some((c, cave)) = &mem {
            c.write_u32(cave + rules::OFF_LEASE, rules::LEASE); // keep the input freeze alive while we run
        }
        let (writes, notices, out, overlay) = ctl.tick(now, &block, life, pos, key);
        if let Some((c, cave)) = &mem {
            for w in writes {
                let ok = match w {
                    Write::Mode(m) => c.write_u32(cave + rules::OFF_MODE, m),
                    Write::State(s) => c.write_u32(cave + rules::OFF_STATE, s),
                    Write::Life(l) if block.life_ptr != 0 => c.write_u32(block.life_ptr, l as u32),
                    Write::Life(_) => false,
                };
                if !ok {
                    tracing::warn!(?w, "downed: write into Cemu failed");
                }
            }
        }
        for m in out {
            let _ = out_tx.send(m);
        }
        if now.duration_since(last_status) >= Duration::from_millis(250) {
            last_status = now;
            let _ = out_tx.send(ctl.status(pos));
        }
        for n in notices {
            tracing::info!(?n, "downed");
            let _ = events.send(Event::Notice(n));
        }
        overlay_changed(&events, &mut shown, overlay);
    }
    // Session over: back to vanilla deaths.
    if let Some((c, cave)) = &mem {
        c.write_u32(cave + rules::OFF_STATE, 0);
        c.write_u32(cave + rules::OFF_MODE, 0);
    }
    overlay_changed(&events, &mut shown, Overlay::default());
}

/// The running Cemu at `cemu_exe`: pid and a read/write process handle (the caller closes it).
#[cfg(windows)]
pub fn open_cemu(cemu_exe: &Path) -> Option<(u32, windows_sys::Win32::Foundation::HANDLE)> {
    mem::open_process(cemu_exe)
}

fn attach(cemu_root: &Path) -> Option<(mem::Cemu, u32)> {
    let log = std::fs::read_to_string(cemu_root.join("log.txt")).ok()?;
    let (base, cave) = rules::parse_cemu_log(&log)?;
    let c = mem::Cemu::open(&cemu_root.join("Cemu.exe"), base)?;
    // Only trust the block once it carries our magic (the log may be from an earlier boot).
    c.read(cave, 4).filter(|b| b[..] == rules::MAGIC.to_be_bytes())?;
    tracing::info!(cave = format_args!("{cave:#x}"), "downed: attached to Cemu");
    Some((c, cave))
}

/// Cemu guest memory access (shared with the shared-enemies sidecar).
#[cfg(windows)]
pub mod mem {
    use std::path::Path;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, STILL_ACTIVE},
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS},
            },
            Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
                PROCESS_VM_WRITE, QueryFullProcessImageNameW,
            },
        },
        UI::{
            Input::KeyboardAndMouse::GetAsyncKeyState,
            WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
        },
    };

    // kernel32's WriteProcessMemory toggles page protection around each write; inside Cemu's single 3.3 GB guest
    // region that costs ~12 ms per call (measured 2026-10-20). NtWriteVirtualMemory writes the read-write pages directly.
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtWriteVirtualMemory(h: HANDLE, addr: *mut core::ffi::c_void, buf: *const core::ffi::c_void, n: usize, written: *mut usize) -> i32;
    }

    /// An open Cemu process and the host address of its guest memory.
    pub struct Cemu {
        h: HANDLE,
        pid: u32,
        base: u64,
    }

    // SAFETY: the handle is a process handle, usable from any thread.
    unsafe impl Send for Cemu {}

    impl Drop for Cemu {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.h) };
        }
    }

    fn image_path(h: HANDLE) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buf/len describe a writable buffer.
        (unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0)
            .then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }

    impl Cemu {
        /// Open the running Cemu whose exe is `exe` (case-insensitive path match).
        pub fn open(exe: &Path, base: u64) -> Option<Cemu> {
            open_process(exe).map(|(pid, h)| Cemu { h, pid, base })
        }
    }

    /// The running Cemu whose exe is `exe` (case-insensitive path match): its pid and a read/write process handle the
    /// caller owns.
    pub fn open_process(exe: &Path) -> Option<(u32, HANDLE)> {
        {
            // Compare case- and slash-insensitively (callers may build the path with either separator).
            let norm = |p: &str| p.to_lowercase().replace('/', "\\");
            let want = norm(&exe.to_string_lossy());
            // SAFETY: plain Toolhelp enumeration; the snapshot handle is closed below.
            let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snap.is_null() || snap as isize == -1 {
                return None;
            }
            let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut found = None;
            let mut ok = unsafe { Process32FirstW(snap, &mut e) } != 0;
            while ok {
                let name = String::from_utf16_lossy(&e.szExeFile[..e.szExeFile.iter().position(|&c| c == 0).unwrap_or(260)]);
                if name.eq_ignore_ascii_case("Cemu.exe") {
                    let access = PROCESS_VM_READ | PROCESS_VM_WRITE | PROCESS_VM_OPERATION | PROCESS_QUERY_LIMITED_INFORMATION;
                    let h = unsafe { OpenProcess(access, 0, e.th32ProcessID) };
                    if !h.is_null() {
                        if image_path(h).is_some_and(|p| norm(&p) == want) {
                            found = Some((e.th32ProcessID, h));
                            break;
                        }
                        unsafe { CloseHandle(h) };
                    }
                }
                ok = unsafe { Process32NextW(snap, &mut e) } != 0;
            }
            unsafe { CloseHandle(snap) };
            found
        }
    }

    impl Cemu {
        pub fn alive(&self) -> bool {
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(self.h, &mut code) != 0 && code == STILL_ACTIVE as u32 }
        }

        pub fn read(&self, guest: u32, n: usize) -> Option<Vec<u8>> {
            let mut buf = vec![0u8; n];
            let mut got = 0usize;
            // SAFETY: reads into our own buffer of n bytes.
            let ok = unsafe { ReadProcessMemory(self.h, (self.base + u64::from(guest)) as *const _, buf.as_mut_ptr().cast(), n, &mut got) };
            (ok != 0 && got == n).then_some(buf)
        }

        pub fn read_u32(&self, guest: u32) -> Option<u32> {
            self.read(guest, 4).map(|b| u32::from_be_bytes(b[..4].try_into().unwrap()))
        }

        pub fn write_u32(&self, guest: u32, v: u32) -> bool {
            self.write(guest, &v.to_be_bytes())
        }

        /// Write raw bytes at a guest address.
        pub fn write(&self, guest: u32, b: &[u8]) -> bool {
            let mut put = 0usize;
            // SAFETY: writes b.len() bytes from our buffer into the guest's memory.
            let ok = unsafe { NtWriteVirtualMemory(self.h, (self.base + u64::from(guest)) as *mut _, b.as_ptr().cast(), b.len(), &mut put) };
            ok >= 0 && put == b.len()
        }

        /// Cemu's window has the keyboard (so G typed into chat never revives anyone).
        pub fn is_foreground(&self) -> bool {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
            pid == self.pid
        }
    }

    pub fn key_down(vk: i32) -> bool {
        (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
    }
}

#[cfg(not(windows))]
pub mod mem {
    use std::path::Path;
    pub struct Cemu;
    impl Cemu {
        pub fn open(_: &Path, _: u64) -> Option<Cemu> {
            None
        }
        pub fn alive(&self) -> bool {
            false
        }
        pub fn read(&self, _: u32, _: usize) -> Option<Vec<u8>> {
            None
        }
        pub fn read_u32(&self, _: u32) -> Option<u32> {
            None
        }
        pub fn write_u32(&self, _: u32, _: u32) -> bool {
            false
        }
        pub fn write(&self, _: u32, _: &[u8]) -> bool {
            false
        }
        pub fn is_foreground(&self) -> bool {
            false
        }
    }
    pub fn key_down(_: i32) -> bool {
        false
    }
}

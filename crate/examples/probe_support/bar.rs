//! A plain always-on-top status bar for `botw_downed_probe` (the app has its own overlay window): one line of text and
//! a progress bar, top-centre of the primary screen, click-through and never focused. Windows only.

use std::sync::{Arc, Mutex, OnceLock};

use privcord_botw_coop::botw_downed::Overlay;

/// What the bar shows: a line of text (empty = hidden) and an optional fill in percent.
pub type Shared = Arc<Mutex<(String, Option<u8>)>>;

static STATE: OnceLock<Shared> = OnceLock::new();

/// Text and progress for an overlay state, worded like the app's overlay.
pub fn describe(o: &Overlay) -> (String, Option<u8>) {
    if o.downed {
        return match &o.revived_by {
            Some((by, p)) => (format!("YOU ARE DOWN - {by} is reviving you"), Some(*p)),
            None if !o.friends_down.is_empty() => ("YOU ARE DOWN - everyone else is down too".into(), None),
            None => ("YOU ARE DOWN - wait for a friend to revive you".into(), None),
        };
    }
    if let Some((name, p)) = &o.prompt {
        return if *p > 0 {
            (format!("Reviving {name}... keep holding G"), Some(*p))
        } else {
            (format!("Hold G to revive {name}"), Some(0))
        };
    }
    if !o.friends_down.is_empty() {
        return (format!("{} is down - go to them and hold G", o.friends_down.join(", ")), None);
    }
    (String::new(), None)
}

/// Start the bar's window thread. Update it through the returned handle.
pub fn start() -> Shared {
    let s = STATE.get_or_init(|| Arc::new(Mutex::new((String::new(), None)))).clone();
    #[cfg(windows)]
    std::thread::spawn(win::run);
    s
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateFontW, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect,
            FrameRect, InvalidateRect, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, EnumWindows, GetWindowRect, GetWindowTextW, HWND_TOPMOST, IsWindowVisible, SWP_NOACTIVATE,
            SWP_NOSIZE, SetWindowPos, DispatchMessageW, GetClientRect, GetMessageW, GetSystemMetrics, LWA_ALPHA, MSG, RegisterClassW,
            SM_CXSCREEN, SW_HIDE, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, WM_PAINT, WM_TIMER,
            WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn rgb(r: u8, g: u8, b: u8) -> u32 {
        u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16
    }

    /// The visible top-level window whose title starts with "Cemu " (Cemu's main window).
    fn cemu_rect() -> Option<RECT> {
        unsafe extern "system" fn each(h: HWND, l: LPARAM) -> windows_sys::core::BOOL {
            unsafe {
                let mut buf = [0u16; 64];
                let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
                if n > 0 && IsWindowVisible(h) != 0 && String::from_utf16_lossy(&buf[..n as usize]).starts_with("Cemu ") {
                    *(l as *mut HWND) = h;
                    return 0;
                }
            }
            1
        }
        let mut found: HWND = std::ptr::null_mut();
        unsafe {
            EnumWindows(Some(each), &mut found as *mut HWND as LPARAM);
            if found.is_null() {
                return None;
            }
            let mut r: RECT = std::mem::zeroed();
            (GetWindowRect(found, &mut r) != 0).then_some(r)
        }
    }

    unsafe extern "system" fn proc_(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        let (text, pct) = super::STATE.get().map(|s| s.lock().unwrap().clone()).unwrap_or_default();
        match msg {
            WM_TIMER => {
                unsafe {
                    // Sit at the top centre of Cemu's game area, wherever its window is.
                    if let Some(r) = cemu_rect() {
                        let mut me: RECT = std::mem::zeroed();
                        GetWindowRect(h, &mut me);
                        let bw = me.right - me.left;
                        SetWindowPos(h, HWND_TOPMOST, r.left + (r.right - r.left - bw) / 2, r.top + 70, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                    }
                    ShowWindow(h, if text.is_empty() { SW_HIDE } else { SW_SHOWNOACTIVATE });
                    InvalidateRect(h, std::ptr::null(), 1);
                }
                0
            }
            WM_PAINT => unsafe {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(h, &mut ps);
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(h, &mut rc);
                let bg = CreateSolidBrush(rgb(20, 20, 24));
                FillRect(dc, &rc, bg);
                DeleteObject(bg);
                let font = CreateFontW(26, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 0, 0, wide("Segoe UI").as_ptr());
                let old = SelectObject(dc, font);
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(dc, rgb(255, 255, 255));
                let mut trc = RECT { left: 10, top: 6, right: rc.right - 10, bottom: 38 };
                let t = wide(&text);
                DrawTextW(dc, t.as_ptr(), -1, &mut trc, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
                SelectObject(dc, old);
                DeleteObject(font);
                if let Some(p) = pct {
                    let outer = RECT { left: 14, top: 44, right: rc.right - 14, bottom: 64 };
                    let frame = CreateSolidBrush(rgb(160, 160, 160));
                    FrameRect(dc, &outer, frame);
                    DeleteObject(frame);
                    let width = (outer.right - outer.left - 4) * i32::from(p.min(100)) / 100;
                    let fill = RECT { left: outer.left + 2, top: outer.top + 2, right: outer.left + 2 + width, bottom: outer.bottom - 2 };
                    let green = CreateSolidBrush(rgb(60, 200, 90));
                    FillRect(dc, &fill, green);
                    DeleteObject(green);
                }
                EndPaint(h, &ps);
                0
            },
            _ => unsafe { DefWindowProcW(h, msg, w, l) },
        }
    }

    pub fn run() {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let class = wide("PrivCordProbeBar");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: inst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            let (w, h) = (640, 72);
            let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
            let title = wide("PrivCord revive");
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                x,
                40,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                inst,
                std::ptr::null(),
            );
            SetLayeredWindowAttributes(hwnd, 0, 235, LWA_ALPHA);
            SetTimer(hwnd, 1, 100, None);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

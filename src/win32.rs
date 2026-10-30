//! Windows backend (windows crate 0.59).
//!
//! Compatibility choices, each verified empirically against the original
//! Go implementations:
//! - Per-Monitor V2 DPI awareness at startup: every rect API (GetWindowRect,
//!   SetWindowPos, MONITORINFO, DWM frame bounds) then agrees in physical
//!   pixels, so no `resizeForDpi` conversion is needed. (RectangleWinPlus
//!   fixed its scaled-screen bugs the same way; we adopt the fixed form.)
//! - `zonable` deliberately does NOT reject WS_POPUP windows. RectangleWin
//!   borrowed a PowerToys FancyZones heuristic that rejects
//!   WS_POPUP|WS_THICKFRAME windows without min/max boxes — which is exactly
//!   Qt's style (QuteBrowser), so RectangleWin silently refused to move it.
//!   WezTerm (WS_POPUP, no caption) has the same problem with the OS-built-in
//!   Win+arrow snap. Foreground + SetWindowPos works for both; that is all
//!   this tool needs.
//! - DWM EXTENDED_FRAME_BOUNDS compensates invisible borders so snapped
//!   windows align exactly with the work area (ported from RectangleWin).

use crate::config::{self, Action, Binding, Config};
use crate::dialogs;
use crate::keys::{Combo, Key};
use crate::layout::{Rect, SizeSpec};
use crate::mouse;
use crate::platform::{App, Monitor, Platform, SizerPick, WinId};
use crate::tray::{self, Cmd, State, Tray};
use crate::vdesk;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SendInput, UnregisterHotKey, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
    MOD_WIN, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, DestroyWindow, DispatchMessageW,
    EnumWindows, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    IsZoomed, KillTimer, MessageBoxW, PostMessageW, SetForegroundWindow, SetProcessDPIAware,
    SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, GA_ROOT, GWL_EXSTYLE, HWND_BOTTOM,
    HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, MB_ICONWARNING, MB_OK, MF_STRING, MSG, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE, SW_NORMAL, SW_RESTORE,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_TOPALIGN, WINDOW_EX_STYLE, WM_HOTKEY, WM_NULL,
    WM_TIMER, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub(crate) fn hwnd_of(id: WinId) -> HWND {
    HWND(id as *mut core::ffi::c_void)
}

pub(crate) fn id_of(hwnd: HWND) -> WinId {
    hwnd.0 as usize as u64
}

/// Bring `hwnd` to the foreground (shared with the mouse-drag hook).
pub(crate) fn focus_window(hwnd: HWND) -> Result<()> {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return Ok(());
        }
        // Foreground lock: attach to the current foreground thread's input
        // queue, retry, then detach.
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let this = GetCurrentThreadId();
        let _ = AttachThreadInput(this, fg_thread, true);
        let ok = SetForegroundWindow(hwnd).as_bool();
        let _ = AttachThreadInput(this, fg_thread, false);
        anyhow::ensure!(ok, "SetForegroundWindow was refused");
    }
    Ok(())
}

/// Per-monitor V2 first, then per-monitor, then the legacy system-DPI call.
pub fn set_dpi_aware() {
    unsafe {
        for ctx in [
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
        ] {
            if SetProcessDpiAwarenessContext(ctx).is_ok() {
                return;
            }
        }
        let _ = SetProcessDPIAware();
    }
}

/// The relaxed window-eligibility check (see module docs).
pub(crate) fn zonable(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        if GetAncestor(hwnd, GA_ROOT) != hwnd
            || !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
        {
            return false;
        }
        // Desktop / shell surfaces. (GetShellWindow is intentionally not
        // used; the class list covers it and avoids an extra API.)
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut buf);
        if n == 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&buf[..n as usize]);
        if matches!(
            class.as_str(),
            "Shell_TrayWnd"
                | "Shell_SecondaryTrayWnd"
                | "Progman"
                | "WorkerW"
                | "SysListView32"
                | "ForegroundStaging"
        ) {
            return false;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        if ex & WS_EX_TOOLWINDOW.0 as i32 != 0 {
            return false;
        }
        true
    }
}

unsafe extern "system" fn enum_topmost(
    hwnd: HWND,
    lparam: LPARAM,
) -> windows::Win32::Foundation::BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    let pinned = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } & WS_EX_TOPMOST.0 as i32 != 0;
    if pinned && zonable(hwnd) {
        list.push(hwnd);
    }
    windows::Win32::Foundation::BOOL(1)
}

/// Windows has no always-on-bottom flag: `toggleAlwaysOnBottom` remembers the
/// lowered HWNDs here and the daemon's 1 s timer re-asserts HWND_BOTTOM — the
/// same re-assert trick as the screen power-off, since any activation would
/// otherwise raise the window again.
static BOTTOM_WINDOWS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn bottom_windows() -> std::sync::MutexGuard<'static, Vec<isize>> {
    BOTTOM_WINDOWS.lock().unwrap_or_else(|e| e.into_inner())
}

fn lower_to_bottom(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Re-assert every tracked always-on-bottom window; TIMER_BOTTOM drives the
/// period (`bottomReassertMs`).
pub fn tick_always_on_bottom() {
    bottom_windows().retain(|&raw| {
        let hwnd = hwnd_of(raw as WinId);
        let alive = unsafe { IsWindow(Some(hwnd)) }.as_bool();
        if alive {
            lower_to_bottom(hwnd);
        }
        alive
    });
}

fn image_basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Lowercased image file name of `pid` (`wezterm-gui.exe`), for matching the
/// windows of multi-process apps in `same_app_windows`.
fn exe_basename(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return None;
        };
        let mut buf = [0u16; 260];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n)
            .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        let name = image_basename(&String::from_utf16_lossy(&buf[..n as usize])).to_string();
        (!name.is_empty()).then_some(name)
    }
}

/// EnumWindows callback context for the exe-name fallback in
/// `same_app_windows`. A named struct, not a tuple: the callback's cast and
/// the caller's layout must stay in lockstep (a `(String, &mut Vec<HWND>)`
/// passed as `(String, Vec<HWND>)` once corrupted the heap here).
struct ExeCtx {
    exe: String,
    out: Vec<HWND>,
}

unsafe extern "system" fn enum_same_exe(
    hwnd: HWND,
    lparam: LPARAM,
) -> windows::Win32::Foundation::BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut ExeCtx) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if zonable(hwnd) && exe_basename(pid).as_deref() == Some(ctx.exe.as_str()) {
        ctx.out.push(hwnd);
    }
    windows::Win32::Foundation::BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, HWND_TOP, PM_REMOVE, WS_VISIBLE};

    #[test]
    fn image_basename_takes_the_part_after_the_last_separator() {
        assert_eq!(
            image_basename(r"C:\Tools\WezTerm\wezterm-gui.exe"),
            "wezterm-gui.exe"
        );
        assert_eq!(
            image_basename(r"C:/Tools/wezterm-gui.exe"),
            "wezterm-gui.exe"
        );
        assert_eq!(image_basename("wezterm-gui.exe"), "wezterm-gui.exe");
    }

    /// A small real window for the desktop-integration tests.
    fn test_window() -> HWND {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("STATIC"),
                windows::core::w!(""),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                200,
                100,
                None,
                None,
                None,
                None,
            )
            .expect("create test window")
        }
    }

    /// All top-level HWNDs, top of the Z-order first.
    fn z_order() -> Vec<HWND> {
        unsafe extern "system" fn cb(
            hwnd: HWND,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::BOOL {
            let v = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
            v.push(hwnd);
            windows::Win32::Foundation::BOOL(1)
        }
        let mut v = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
        }
        v
    }

    /// Regression: the exe-name fallback's EnumWindows callback pushed through
    /// a mismatched tuple cast and corrupted the heap (daemon crashed on
    /// Alt+`). Enumerating real windows exercises the exact callback path.
    #[test]
    #[ignore = "creates real windows on the desktop"]
    fn exe_fallback_enumerates_windows_of_our_own_process() {
        let a = test_window();
        let exe = exe_basename(std::process::id()).expect("own process image");
        let mut ctx = ExeCtx {
            exe,
            out: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(enum_same_exe), LPARAM(&mut ctx as *mut _ as isize));
        }
        assert!(ctx.out.contains(&a), "fallback must find our own window");
        unsafe {
            let _ = DestroyWindow(a);
        }
    }

    /// `toggleAlwaysOnBottom`'s mechanism: lower_to_bottom sinks the window,
    /// and the timer tick pushes it back down after something raises it.
    #[test]
    #[ignore = "creates real windows on the desktop"]
    fn always_on_bottom_sinks_and_reasserts_on_tick() {
        let a = test_window();
        let b = test_window();
        let pos = |h: HWND| z_order().iter().position(|&w| w == h).expect("in Z order");

        lower_to_bottom(a);
        assert!(pos(a) > pos(b), "a must sink below b");

        unsafe {
            let _ = SetWindowPos(
                a,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        assert!(pos(a) < pos(b), "a is raised again");
        bottom_windows().push(a.0 as isize);
        tick_always_on_bottom();
        assert!(pos(a) > pos(b), "tick must re-assert HWND_BOTTOM");

        bottom_windows().clear();
        unsafe {
            let _ = DestroyWindow(a);
            let _ = DestroyWindow(b);
        }
    }

    /// The trait-level toggle the hotkey dispatches to: on = sink + track,
    /// off = untrack. Uses another process's window style of call (trait
    /// method on Win32), not the free helpers.
    #[test]
    #[ignore = "creates real windows on the desktop"]
    fn platform_toggle_always_on_bottom_tracks_and_untracks() {
        let p = init();
        let a = test_window();
        let b = test_window();
        let id = id_of(a);
        let pos = |h: HWND| z_order().iter().position(|&w| w == h).expect("in Z order");

        assert!(!p.is_always_on_bottom(id));
        p.set_always_on_bottom(id, true).expect("sink");
        assert!(p.is_always_on_bottom(id), "tracked after on");
        assert!(pos(a) > pos(b), "a must sink below b");

        p.set_always_on_bottom(id, false).expect("restore");
        assert!(!p.is_always_on_bottom(id), "untracked after off");
        assert!(
            pos(a) < pos(b),
            "undo raises the just-focused window on top"
        );

        unsafe {
            let _ = DestroyWindow(a);
            let _ = DestroyWindow(b);
        }
    }

    /// The daemon matches WM_TIMER by the id SetTimer RETURNS: with a NULL
    /// window the requested TIMER_BOTTOM id is ignored and Windows generates
    /// its own. Matching on the constant instead meant the re-assert tick
    /// never ran in the daemon (a bottomed window raised by a click stayed
    /// raised forever). This test pumps the real message loop.
    #[test]
    #[ignore = "real desktop: pumps thread timer messages"]
    fn bottom_timer_wm_timer_carries_the_returned_id() {
        let id = reset_bottom_timer(0, 50);
        assert_ne!(id, 0, "SetTimer must succeed");
        let mut seen = false;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) && !seen {
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                if msg.message == WM_TIMER && msg.wParam.0 == id {
                    seen = true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Re-arm (the config-reload path) must also return a live id.
        let id2 = reset_bottom_timer(id, 50);
        assert_ne!(id2, 0);
        unsafe {
            let _ = KillTimer(None, id2);
        }
        assert!(seen, "no WM_TIMER arrived with the id SetTimer returned");
    }
}

pub(crate) fn rect_of(r: RECT) -> Rect {
    Rect {
        l: r.left,
        t: r.top,
        r: r.right,
        b: r.bottom,
    }
}

pub(crate) struct MonitorRow {
    pub(crate) handle: HMONITOR,
    pub(crate) work: Rect,
}

pub(crate) fn enum_monitors() -> Vec<MonitorRow> {
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        let v = &mut *(lparam.0 as *mut Vec<MonitorRow>);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            v.push(MonitorRow {
                handle: hmon,
                work: rect_of(mi.rcWork),
            });
        }
        windows::Win32::Foundation::BOOL(1)
    }
    let mut v: Vec<MonitorRow> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Poll thread state shared with the platform: last zonable foreground HWND.
#[derive(Clone)]
pub struct LastActive {
    inner: Arc<AtomicU64>,
}

impl LastActive {
    fn new() -> Self {
        let la = LastActive {
            inner: Arc::new(AtomicU64::new(0)),
        };
        let inner = la.inner.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let hwnd = unsafe { GetForegroundWindow() };
            if zonable(hwnd) {
                inner.store(id_of(hwnd), Ordering::SeqCst);
            }
        });
        la
    }
    fn get(&self) -> Option<WinId> {
        let id = self.inner.load(Ordering::SeqCst);
        (id != 0).then_some(id)
    }
}

pub struct Win32 {
    last_active: LastActive,
    hotkeys: Vec<(i32, ActionId)>,
}

/// Binding index doubles as the hotkey id carrier inside the message loop.
type ActionId = usize;

pub fn init() -> Win32 {
    set_dpi_aware();
    Win32 {
        last_active: LastActive::new(),
        hotkeys: Vec::new(),
    }
}

pub(crate) fn vk_of_key(key: Key) -> Option<VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    Some(match key {
        Key::Char('`') => VK_OEM_3,
        Key::Char(';') => VK_OEM_1,
        Key::Char('=') => VK_OEM_PLUS,
        Key::Char(',') => VK_OEM_COMMA,
        Key::Char('-') => VK_OEM_MINUS,
        Key::Char('.') => VK_OEM_PERIOD,
        Key::Char('/') => VK_OEM_2,
        Key::Char('[') => VK_OEM_4,
        Key::Char('\\') => VK_OEM_5,
        Key::Char(']') => VK_OEM_6,
        Key::Char('\'') => VK_OEM_7,
        Key::Char(c) => VIRTUAL_KEY(c.to_ascii_uppercase() as u16),
        Key::F(1) => VK_F1,
        Key::F(2) => VK_F2,
        Key::F(3) => VK_F3,
        Key::F(4) => VK_F4,
        Key::F(5) => VK_F5,
        Key::F(6) => VK_F6,
        Key::F(7) => VK_F7,
        Key::F(8) => VK_F8,
        Key::F(9) => VK_F9,
        Key::F(10) => VK_F10,
        Key::F(11) => VK_F11,
        Key::F(12) => VK_F12,
        Key::F(n) => VIRTUAL_KEY(VK_F12.0 + (n - 12) as u16),
        Key::Insert => VK_INSERT,
        Key::Delete => VK_DELETE,
        Key::Space => VK_SPACE,
        Key::Tab => VK_TAB,
        Key::Backspace => VK_BACK,
        Key::Escape => VK_ESCAPE,
        Key::Enter => VK_RETURN,
        Key::Left => VK_LEFT,
        Key::Right => VK_RIGHT,
        Key::Up => VK_UP,
        Key::Down => VK_DOWN,
        Key::Home => VK_HOME,
        Key::End => VK_END,
        Key::PageUp => VK_PRIOR,
        Key::PageDown => VK_NEXT,
        Key::Snapshot => VK_SNAPSHOT,
        Key::Pause => VK_PAUSE,
        Key::Other => return None,
    })
}

fn modifiers_of(c: &Combo) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT;
    if c.ctrl {
        m |= MOD_CONTROL;
    }
    if c.shift {
        m |= MOD_SHIFT;
    }
    if c.alt {
        m |= MOD_ALT;
    }
    if c.win {
        m |= MOD_WIN;
    }
    m
}

/// Re-inject a combo that the daemon swallowed but turned out not to own, so
/// the foreground app still sees a normal keystroke (DoubleCmd's
/// ctrl+alt+l). The physical modifiers may still be held; the extra
/// down/up events are harmless to the receiving app.
fn pass_through(c: &Combo) {
    let Some(vk) = vk_of_key(c.key) else {
        return;
    };
    let mut inputs = Vec::with_capacity(10);
    let mut press = |vk: VIRTUAL_KEY, up: bool| {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        });
    };
    if c.ctrl {
        press(VK_CONTROL, false);
    }
    if c.shift {
        press(VK_SHIFT, false);
    }
    if c.alt {
        press(VK_MENU, false);
    }
    if c.win {
        press(VK_LWIN, false);
    }
    press(vk, false);
    press(vk, true);
    if c.win {
        press(VK_LWIN, true);
    }
    if c.alt {
        press(VK_MENU, true);
    }
    if c.shift {
        press(VK_SHIFT, true);
    }
    if c.ctrl {
        press(VK_CONTROL, true);
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    // TEMP DEBUG (remove after alt+shift+l diagnosis)
    {
        use windows::Win32::Foundation::GetLastError;
        use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};
        unsafe {
            let fg = GetForegroundWindow();
            let mut buf = [0u16; 256];
            let n = GetClassNameW(fg, &mut buf);
            let cls = String::from_utf16_lossy(&buf[..n as usize]);
            let line = format!(
                "pass_through combo={c:?} inputs={} sent={sent} err={:?} fg={cls}\n",
                inputs.len(),
                GetLastError()
            );
            let paths = [
                std::env::temp_dir().join("dt-passthrough.log"),
                std::env::current_exe()
                    .ok()
                    .map(|p| p.with_file_name("dt-passthrough.log"))
                    .unwrap_or_default(),
            ];
            for path in paths {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    use std::io::Write as _;
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }
}

impl Platform for Win32 {
    fn monitors(&self) -> Vec<Monitor> {
        enum_monitors()
            .into_iter()
            .map(|m| Monitor { work: m.work })
            .collect()
    }

    fn monitor_index(&self, id: WinId) -> usize {
        let rows = enum_monitors();
        if rows.is_empty() {
            return 0;
        }
        let cur = unsafe { MonitorFromWindow(hwnd_of(id), MONITOR_DEFAULTTONEAREST) };
        rows.iter().position(|r| r.handle == cur).unwrap_or(0)
    }

    fn foreground(&self) -> Option<WinId> {
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.0.is_null()).then(|| id_of(hwnd))
    }

    fn last_active(&self) -> Option<WinId> {
        self.last_active.get()
    }

    fn is_zonable(&self, id: WinId) -> bool {
        zonable(hwnd_of(id))
    }

    fn window_rect(&self, id: WinId) -> Option<Rect> {
        let mut rc = RECT::default();
        unsafe { GetWindowRect(hwnd_of(id), &mut rc) }.ok()?;
        Some(rect_of(rc))
    }

    fn visible_frame(&self, id: WinId) -> Option<Rect> {
        let mut rc = RECT::default();
        let r = unsafe {
            DwmGetWindowAttribute(
                hwnd_of(id),
                DWMWINDOWATTRIBUTE(9), // DWMWA_EXTENDED_FRAME_BOUNDS
                &mut rc as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<RECT>() as u32,
            )
        };
        if r.is_err() {
            return None;
        }
        let rr = rect_of(rc);
        rr.has_area().then_some(rr)
    }

    fn move_window(&self, id: WinId, r: Rect) -> Result<()> {
        let hwnd = hwnd_of(id);
        unsafe {
            // Un-maximize first, like RectangleWin, so SetWindowPos sticks.
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_NORMAL);
            }
            SetWindowPos(
                hwnd,
                None,
                r.l,
                r.t,
                r.w(),
                r.h(),
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .context("SetWindowPos failed")?;
        }
        Ok(())
    }

    fn is_maximized(&self, id: WinId) -> bool {
        unsafe { IsZoomed(hwnd_of(id)).as_bool() }
    }

    fn set_maximized(&self, id: WinId, on: bool) -> Result<()> {
        let cmd = if on { SW_MAXIMIZE } else { SW_RESTORE };
        unsafe {
            let _ = ShowWindow(hwnd_of(id), cmd);
        }
        Ok(())
    }

    fn is_always_on_top(&self, id: WinId) -> bool {
        unsafe { GetWindowLongW(hwnd_of(id), GWL_EXSTYLE) & WS_EX_TOPMOST.0 as i32 != 0 }
    }

    fn set_always_on_top(&self, id: WinId, on: bool) -> Result<()> {
        let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
        unsafe {
            SetWindowPos(
                hwnd_of(id),
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE,
            )
            .context("SetWindowPos(TOPMOST) failed")?;
        }
        Ok(())
    }

    fn is_always_on_bottom(&self, id: WinId) -> bool {
        bottom_windows().contains(&(hwnd_of(id).0 as isize))
    }

    fn set_always_on_bottom(&self, id: WinId, on: bool) -> Result<()> {
        let hwnd = hwnd_of(id);
        let raw = hwnd.0 as isize;
        let mut tracked = bottom_windows();
        if on {
            if !tracked.contains(&raw) {
                tracked.push(raw);
            }
            lower_to_bottom(hwnd);
        } else {
            tracked.retain(|&h| h != raw);
            // Undo always targets the window the user just focused: raise it
            // to the top (unpinned), like a normal focus would.
            unsafe {
                SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)
                    .context("SetWindowPos(HWND_TOP) failed")?;
            }
        }
        Ok(())
    }

    fn minimize(&self, id: WinId) -> Result<()> {
        unsafe {
            let _ = ShowWindow(hwnd_of(id), SW_MINIMIZE);
        }
        Ok(())
    }

    fn restore(&self, id: WinId) -> Result<()> {
        unsafe {
            let _ = ShowWindow(hwnd_of(id), SW_RESTORE);
        }
        Ok(())
    }

    fn unpin_all(&self) -> Result<()> {
        let mut topmost: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_topmost), LPARAM(&mut topmost as *mut _ as isize));
        }
        for hwnd in topmost {
            if let Err(e) = self.set_always_on_top(id_of(hwnd), false) {
                eprintln!("warn: unpin_all: {}: {e:#}", hwnd.0 as usize);
            }
        }
        Ok(())
    }

    fn same_app_windows(&self, id: WinId) -> Vec<WinId> {
        struct Ctx {
            pid: u32,
            out: Vec<HWND>,
        }
        unsafe extern "system" fn cb(
            hwnd: HWND,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == ctx.pid && zonable(hwnd) {
                ctx.out.push(hwnd);
            }
            windows::Win32::Foundation::BOOL(1)
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd_of(id), Some(&mut pid)) };
        if pid == 0 {
            return vec![id];
        }
        let mut ctx = Ctx {
            pid,
            out: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
        }
        if ctx.out.len() > 1 {
            return ctx.out.into_iter().map(id_of).collect();
        }
        // One window per process: each WezTerm window is its own
        // wezterm-gui.exe, so the PID match finds only the foreground window.
        // Fall back to the executable image name to still cycle the app's
        // windows.
        let Some(exe) = exe_basename(pid) else {
            return vec![id];
        };
        let mut exe_ctx = ExeCtx {
            exe,
            out: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(enum_same_exe), LPARAM(&mut exe_ctx as *mut _ as isize));
        }
        if exe_ctx.out.len() > 1 {
            return exe_ctx.out.into_iter().map(id_of).collect();
        }
        vec![id]
    }

    fn focus(&self, id: WinId) -> Result<()> {
        focus_window(hwnd_of(id))
    }

    fn switch_desktop(&self, n: u8) -> Result<()> {
        vdesk::switch_to(n)
    }

    fn move_to_desktop(&self, id: WinId, n: u8) -> Result<()> {
        vdesk::move_window_to(hwnd_of(id), n)
    }

    fn next_desktop(&self) -> Result<()> {
        vdesk::switch_relative(1)
    }

    fn prev_desktop(&self) -> Result<()> {
        vdesk::switch_relative(-1)
    }

    fn toggle_screen(&self) -> Result<()> {
        crate::screen::toggle();
        Ok(())
    }

    fn file_dialog(&self) -> Result<()> {
        dialogs::fill_foreground()
    }

    fn pick_sizer(&self, presets: &[SizeSpec]) -> SizerPick {
        unsafe {
            // The menu must be owned by this process: a foreign foreground
            // window makes TrackPopupMenu return 0 without showing anything.
            let Ok(owner) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("STATIC"),
                windows::core::w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            ) else {
                return SizerPick::Cancelled;
            };
            let Ok(menu) = CreatePopupMenu() else {
                let _ = DestroyWindow(owner);
                return SizerPick::Cancelled;
            };
            for (i, spec) in presets.iter().enumerate() {
                let label = windows::core::HSTRING::from(spec.to_string());
                if AppendMenuW(menu, MF_STRING, i + 1, &label).is_err() {
                    let _ = DestroyMenu(menu);
                    let _ = DestroyWindow(owner);
                    return SizerPick::Cancelled;
                }
            }
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt).is_err() {
                let _ = DestroyMenu(menu);
                let _ = DestroyWindow(owner);
                return SizerPick::Cancelled;
            }
            let cmd = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                None,
                owner,
                None,
            );
            // Without this the menu can stay "stuck" after dismissal.
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            let _ = DestroyWindow(owner);
            match cmd.0 {
                0 => SizerPick::Cancelled,
                id => SizerPick::Picked(id as usize - 1),
            }
        }
    }

    fn find_by_pid(&self, pid: u32) -> Option<WinId> {
        struct Ctx {
            pid: u32,
            found: HWND,
        }
        unsafe extern "system" fn cb(
            hwnd: HWND,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            let mut wpid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut wpid)) };
            if wpid == ctx.pid && zonable(hwnd) {
                ctx.found = hwnd;
                return windows::Win32::Foundation::BOOL(0); // stop enumerating
            }
            windows::Win32::Foundation::BOOL(1)
        }
        let mut ctx = Ctx {
            pid,
            found: HWND(std::ptr::null_mut()),
        };
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
        }
        (!ctx.found.0.is_null()).then(|| id_of(ctx.found))
    }
}

pub fn message_box(text: &str) {
    use windows::core::HSTRING;
    let title = HSTRING::from("desktop-tools-rs");
    let text = HSTRING::from(text);
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | MB_ICONWARNING);
    }
}

/// Register all bindings; returns the combos that failed (already in use).
pub fn register_hotkeys(p: &mut Win32, bindings: &[Binding]) -> Vec<String> {
    let mut failed = Vec::new();
    p.hotkeys.clear();
    for (i, b) in bindings.iter().enumerate() {
        // Win combos live in the kbdhook (Explorer owns the Win+key namespace
        // and registers many of them, so RegisterHotKey fails by design);
        // suppress rows likewise need no registration.
        if b.combo.win || b.action == Action::Suppress {
            continue;
        }
        let Some(vk) = vk_of_key(b.combo.key) else {
            failed.push(format!("{combo} (unknown key)", combo = b.combo));
            continue;
        };
        let id = (i + 1) as i32;
        let ok = unsafe { RegisterHotKey(None, id, modifiers_of(&b.combo), vk.0 as u32) }.is_ok();
        if ok {
            p.hotkeys.push((id, i));
        } else {
            failed.push(b.combo.to_string());
        }
    }
    failed
}

fn unregister_hotkeys(p: &mut Win32) {
    for (id, _) in p.hotkeys.drain(..) {
        unsafe {
            let _ = UnregisterHotKey(None, id);
        }
    }
}

const TIMER_RELOAD: usize = 1;
const TIMER_DIALOGS: usize = 2;
const TIMER_BOTTOM: usize = 3;

/// (Re-)arm the always-on-bottom re-assert timer; the period is a setting, so
/// a config reload calls this with the new value. Returns the timer id that
/// WM_TIMER will carry: with a NULL window the requested TIMER_BOTTOM id is
/// ignored and the system generates the real one, so the caller MUST match on
/// this return value (same convention as the reload/dialog timers).
fn reset_bottom_timer(old: usize, ms: u64) -> usize {
    unsafe {
        if old != 0 {
            let _ = KillTimer(None, old);
        }
        SetTimer(None, TIMER_BOTTOM, ms as u32, None)
    }
}

fn status_text(total: usize, failed: &[String], suspended: bool) -> String {
    if suspended {
        return "desktop-tools-rs — hotkeys suspended".to_string();
    }
    let mut s = format!("desktop-tools-rs — {total} hotkeys");
    if !failed.is_empty() {
        s.push_str(&format!(
            ", {} unavailable: {}",
            failed.len(),
            failed.join(", ")
        ));
    }
    s
}

/// Run one binding's action. `hid` is the RegisterHotKey id for WM_HOTKEY
/// deliveries (needed to re-arm the pass-through combo); hook deliveries
/// pass `None`. Returns true when the daemon should quit.
#[allow(clippy::too_many_arguments)]
fn handle_action(
    app: &mut App<Win32>,
    cfg: &Config,
    tray: &mut Tray,
    presets_path: &Path,
    idx: usize,
    hid: Option<i32>,
) -> bool {
    let action = cfg.bindings[idx].action;
    if action == Action::Quit {
        return true;
    }
    if action == Action::SizerMenu {
        match config::load_presets(presets_path) {
            Ok(p) if !p.is_empty() => app.set_sizer(p),
            Ok(_) => eprintln!("warn: resize.toml has no usable presets; keeping old"),
            Err(e) => eprintln!("warn: cannot read resize.toml, keeping old presets: {e:#}"),
        }
    }
    if action == Action::FileDialog && !dialogs::foreground_is_dialog() {
        // No dialog in front: the combo belongs to the app underneath
        // (DoubleCmd uses ctrl+alt+l for "list view"), so re-inject it
        // instead of swallowing it. Unregister first: injected input
        // re-triggers global hotkeys, which would feed the daemon right back
        // into this handler.
        if let Some(hid) = hid {
            unsafe { UnregisterHotKey(None, hid) }.ok();
        }
        pass_through(&cfg.bindings[idx].combo);
        if let Some(hid) = hid {
            let mods = modifiers_of(&cfg.bindings[idx].combo);
            if let Some(vk) = vk_of_key(cfg.bindings[idx].combo.key) {
                unsafe { RegisterHotKey(None, hid, mods, vk.0 as u32) }.ok();
            }
        }
    } else if let Err(e) = app.apply(action) {
        eprintln!("warn: {action:?}: {e:#}");
        // Release is GUI subsystem, so stderr is invisible when the daemon
        // was started from Explorer/autostart.
        tray.set_status(&format!("desktop-tools-rs — {action:?}: {e:#}"));
    }
    false
}

pub fn run_daemon(cfg_path: PathBuf, mut cfg: Config) -> Result<()> {
    // Single instance (daemon only; one-shots may run alongside it).
    let mutex_name = windows::core::HSTRING::from("Local\\desktop-tools-rs-single-instance");
    let _guard = unsafe { CreateMutexW(None, true, &mutex_name) }.ok();
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        message_box("desktop-tools-rs is already running.");
        return Ok(());
    }

    let mut app = App::new(init());
    app.set_gap(cfg.settings.gap);
    let presets_path = cfg_path.with_file_name("resize.toml");
    let presets = config::load_or_create_presets(&presets_path, include_str!("../resize.toml"))
        .unwrap_or_else(|e| {
            eprintln!("warn: {e:#}");
            Vec::new()
        });
    app.set_sizer(presets);
    let mut tray = match Tray::new() {
        Ok(t) => t,
        Err(e) => {
            // Release is a GUI subsystem, so stderr would go nowhere.
            message_box(&format!("Cannot create the tray icon; exiting.\n\n{e:#}"));
            return Err(e);
        }
    };
    let mut suspended = false;
    // Set while a reload result icon is showing; see the WM_TIMER arm.
    let mut flash: Option<Instant> = None;

    let failed = register_hotkeys(&mut app.p, &cfg.bindings);
    tray.set_status(&status_text(cfg.bindings.len(), &failed, suspended));
    if app.p.hotkeys.is_empty() {
        let msg = config::conflict_message(&failed)
            .unwrap_or_else(|| "No hotkeys could be registered.".to_string());
        message_box(&format!(
            "No hotkeys could be registered; exiting.\n\n{msg}"
        ));
        anyhow::bail!("no hotkeys could be registered; exiting");
    }
    if let Some(msg) = config::conflict_message(&failed) {
        eprintln!("warn: {msg}");
        message_box(&msg);
    }

    // Win+LMB drag-move. Optional: if the hook cannot be installed the
    // hotkeys still work.
    mouse::set_modifiers(cfg.settings.drag_modifier);
    if let Err(e) = mouse::install() {
        eprintln!("warn: {e:#}");
    }

    // Win-containing combos collide with shell-owned hotkeys: Explorer reacts
    // even when RegisterHotKey succeeds, so a keyboard hook suppresses them
    // before the shell sees the trigger key.
    crate::kbdhook::set_bindings(&cfg.bindings);
    if let Err(e) = crate::kbdhook::install() {
        eprintln!("warn: {e:#}");
    }

    // Hot reload: a thread timer keeps the WM_HOTKEY loop free of polling
    // code; each tick just compares the config mtime. With a NULL window the
    // returned id is the one that shows up in WM_TIMER (not necessarily the
    // requested TIMER_RELOAD), so keep it.
    let mut mtime = config::modified(&cfg_path);
    let timer = unsafe { SetTimer(None, TIMER_RELOAD, 1000, None) };
    let dialog_timer = unsafe { SetTimer(None, TIMER_DIALOGS, 250, None) };
    let mut bottom_timer = reset_bottom_timer(0, cfg.settings.bottom_reassert_ms);

    loop {
        let mut msg = MSG::default();
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 <= 0 {
            return Ok(());
        }
        unsafe { DispatchMessageW(&msg) };
        match msg.message {
            WM_HOTKEY => {
                let id = msg.wParam.0 as i32;
                if let Some(&(_, idx)) = app.p.hotkeys.iter().find(|(hid, _)| *hid == id) {
                    if handle_action(&mut app, &cfg, &mut tray, &presets_path, idx, Some(id)) {
                        return Ok(());
                    }
                }
            }
            crate::kbdhook::WM_KBDHOOK => {
                let idx = msg.wParam.0;
                if idx < cfg.bindings.len()
                    && handle_action(&mut app, &cfg, &mut tray, &presets_path, idx, None)
                {
                    return Ok(());
                }
            }
            WM_TIMER if msg.wParam.0 == timer => {
                // Re-assert the monitor power-off and restore windows whose
                // monitor unplugged itself while the screens were off.
                crate::screen::tick();
                // A reload often finishes in a few ms, so hold its icon for at
                // least 1 s. The 1 s timer makes the hold 1-2 s in practice.
                if flash.is_some_and(|t| t.elapsed() >= Duration::from_secs(1)) {
                    flash = None;
                    tray.set_state(if suspended {
                        State::Suspended
                    } else {
                        State::Normal
                    });
                }
                let now = config::modified(&cfg_path);
                if now != mtime {
                    mtime = now;
                    reload(
                        &mut app,
                        &mut cfg,
                        &mut tray,
                        suspended,
                        &cfg_path,
                        &mut bottom_timer,
                    );
                    flash = Some(Instant::now());
                }
            }
            WM_TIMER if msg.wParam.0 == bottom_timer => {
                // Re-assert always-on-bottom windows (activation raises them);
                // the timer's period is the `bottomReassertMs` setting.
                tick_always_on_bottom();
            }
            WM_TIMER if msg.wParam.0 == dialog_timer && !suspended => {
                dialogs::tick(cfg.settings.quick_switch);
            }
            _ => {}
        }
        while let Some(cmd) = tray.poll() {
            match cmd {
                Cmd::ToggleAutostart => {
                    if let Err(e) = tray.toggle_autostart() {
                        eprintln!("warn: autostart: {e:#}");
                        tray.set_status("desktop-tools-rs — cannot change autostart");
                    }
                }
                Cmd::Reload => {
                    reload(
                        &mut app,
                        &mut cfg,
                        &mut tray,
                        suspended,
                        &cfg_path,
                        &mut bottom_timer,
                    );
                    flash = Some(Instant::now());
                    // Avoid re-running the same reload on the next timer tick.
                    mtime = config::modified(&cfg_path);
                }
                Cmd::Edit => {
                    if let Err(e) = tray::edit_config(&cfg_path) {
                        eprintln!("warn: edit config: {e:#}");
                        tray.set_status(&format!("desktop-tools-rs — {e:#}"));
                    }
                }
                Cmd::UnpinAll => {
                    if let Err(e) = app.p.unpin_all() {
                        eprintln!("warn: unpin all: {e:#}");
                    }
                }
                Cmd::ToggleSuspend => {
                    flash = None;
                    if suspended {
                        let failed = register_hotkeys(&mut app.p, &cfg.bindings);
                        if app.p.hotkeys.is_empty() {
                            eprintln!("warn: resume failed: no hotkeys could be registered");
                            tray.set_status("desktop-tools-rs — resume failed, still suspended");
                        } else {
                            suspended = false;
                            tray.set_suspended(false);
                            if let Err(e) = mouse::install() {
                                eprintln!("warn: {e:#}");
                            }
                            crate::kbdhook::set_bindings(&cfg.bindings);
                            if let Err(e) = crate::kbdhook::install() {
                                eprintln!("warn: {e:#}");
                            }
                            if let Some(msg) = config::conflict_message(&failed) {
                                eprintln!("warn: {msg}");
                            }
                            tray.set_status(&status_text(cfg.bindings.len(), &failed, false));
                        }
                    } else {
                        unregister_hotkeys(&mut app.p);
                        mouse::uninstall();
                        crate::kbdhook::uninstall();
                        suspended = true;
                        tray.set_suspended(true);
                        tray.set_status(&status_text(cfg.bindings.len(), &[], true));
                    }
                }
                Cmd::Quit => return Ok(()),
            }
        }
    }
}

/// Re-read the config and swap hotkeys in place. Any failure keeps the old
/// bindings working; conflicts are reported per combo and the rest register.
fn reload(
    app: &mut App<Win32>,
    cfg: &mut Config,
    tray: &mut Tray,
    suspended: bool,
    path: &Path,
    bottom_timer: &mut usize,
) {
    let new = match config::reload(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warn: config reload failed, keeping old config: {e:#}");
            tray.set_status("desktop-tools-rs — reload failed, old config kept");
            tray.set_state(State::ReloadFail);
            return;
        }
    };
    if new.bindings.is_empty() {
        eprintln!("warn: reloaded config has no usable bindings; keeping old config");
        tray.set_status("desktop-tools-rs — reload failed, no usable bindings");
        tray.set_state(State::ReloadFail);
        return;
    }
    if suspended {
        *cfg = new;
        app.set_gap(cfg.settings.gap);
        mouse::set_modifiers(cfg.settings.drag_modifier);
        *bottom_timer = reset_bottom_timer(*bottom_timer, cfg.settings.bottom_reassert_ms);
        tray.set_status("desktop-tools-rs — config reloaded (hotkeys suspended)");
        tray.set_state(State::ReloadOk);
        return;
    }

    let old = std::mem::replace(cfg, new);
    unregister_hotkeys(&mut app.p);
    let failed = register_hotkeys(&mut app.p, &cfg.bindings);
    crate::kbdhook::set_bindings(&cfg.bindings);
    if app.p.hotkeys.is_empty() {
        // Nothing registered: roll back rather than leave the daemon dead.
        eprintln!("warn: reloaded config registered no hotkeys; keeping old config");
        *cfg = old;
        app.set_gap(cfg.settings.gap);
        mouse::set_modifiers(cfg.settings.drag_modifier);
        unregister_hotkeys(&mut app.p);
        let _ = register_hotkeys(&mut app.p, &cfg.bindings);
        crate::kbdhook::set_bindings(&cfg.bindings);
        tray.set_status("desktop-tools-rs — reload failed, old config kept");
        tray.set_state(State::ReloadFail);
        return;
    }
    app.set_gap(cfg.settings.gap);
    mouse::set_modifiers(cfg.settings.drag_modifier);
    *bottom_timer = reset_bottom_timer(*bottom_timer, cfg.settings.bottom_reassert_ms);
    if let Some(msg) = config::conflict_message(&failed) {
        eprintln!("warn: {msg}");
    }
    tray.set_status(&status_text(cfg.bindings.len(), &failed, false));
    tray.set_state(State::ReloadOk);
}

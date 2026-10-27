//! Win+F2 monitor power toggle (a port of toggle-screen-autohotkey) with one
//! fix for multi-monitor setups: powering a DisplayPort monitor off drops the
//! video link, so Windows treats it as unplugged, consolidates its windows
//! onto the primary monitor, and never moves them back on wake. Before
//! powering off we snapshot every zonable window's monitor (identified by
//! work area, so enumeration reordering is harmless) and rect; the 1 s timer
//! then watches the monitor count and puts the windows back when the
//! disconnected monitors return.

use crate::layout::Rect;
use crate::win32::{enum_monitors, rect_of, zonable, MonitorRow};
use std::sync::{Mutex, MutexGuard};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindowRect, IsWindow, IsZoomed, PostMessageW, SetWindowPos,
    ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE, SW_NORMAL, WM_SYSCOMMAND,
};

/// `WM_SYSCOMMAND` monitor power command; `lParam` 2 = off, -1 = on. Posted
/// to the shell window (Progman), like the AutoHotkey script does.
const SC_MONITORPOWER: usize = 0xF170;

struct Saved {
    /// Raw HWND; stored as an integer so the static state stays `Sync`.
    hwnd: isize,
    /// Work area of the monitor the window was on: the monitor's identity.
    work: Rect,
    rect: Rect,
    maximized: bool,
}

struct State {
    /// User toggle: while set, the timer keeps re-asserting the power-off, so
    /// any stray wake input re-darkens the screens within a second.
    forced_off: bool,
    /// Monitor count at snapshot time; a smaller count means a monitor
    /// reported itself unplugged while the screens were off.
    expected: usize,
    disconnected: bool,
    saved: Vec<Saved>,
}

static STATE: Mutex<State> = Mutex::new(State {
    forced_off: false,
    expected: 0,
    disconnected: false,
    saved: Vec::new(),
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The `toggleScreen` hotkey.
pub fn toggle() {
    let mut st = state();
    if st.forced_off {
        st.forced_off = false;
        st.saved.clear();
        power(-1);
    } else {
        st.saved = snapshot();
        st.expected = enum_monitors().len();
        st.disconnected = false;
        st.forced_off = true;
        power(2);
    }
}

/// Called from the daemon's 1 s timer.
pub fn tick() {
    let mut st = state();
    if st.forced_off {
        power(2);
    }
    if st.saved.is_empty() {
        return;
    }
    let mons = enum_monitors();
    if mons.len() < st.expected {
        st.disconnected = true;
        return;
    }
    if st.disconnected {
        restore(&st.saved, &mons);
        st.saved.clear();
    }
}

fn power(mode: isize) {
    unsafe {
        if let Ok(progman) = FindWindowW(windows::core::w!("Progman"), None) {
            if !progman.0.is_null() {
                let _ = PostMessageW(
                    Some(progman),
                    WM_SYSCOMMAND,
                    WPARAM(SC_MONITORPOWER),
                    LPARAM(mode),
                );
            }
        }
    }
}

fn hwnd_of(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

struct SnapCtx {
    mons: Vec<MonitorRow>,
    out: Vec<Saved>,
}

unsafe extern "system" fn snap_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = unsafe { &mut *(lparam.0 as *mut SnapCtx) };
    if !zonable(hwnd) {
        return BOOL(1);
    }
    let mut rc = windows::Win32::Foundation::RECT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut rc).is_ok() {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if let Some(m) = ctx.mons.iter().find(|m| m.handle == hmon) {
                ctx.out.push(Saved {
                    hwnd: hwnd.0 as isize,
                    work: m.work,
                    rect: rect_of(rc),
                    maximized: IsZoomed(hwnd).as_bool(),
                });
            }
        }
    }
    BOOL(1)
}

fn snapshot() -> Vec<Saved> {
    let mut ctx = SnapCtx {
        mons: enum_monitors(),
        out: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(snap_cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.out
}

/// A saved window must be put back only when its monitor exists again (matched
/// by work area) and the window is not already on it — with no unplug there
/// is nothing to fix, and while the monitor is gone the window must stay put.
fn needs_restore(saved_work: Rect, current_work: Option<Rect>, mons: &[MonitorRow]) -> bool {
    mons.iter().any(|m| m.work == saved_work) && current_work != Some(saved_work)
}

fn restore(saved: &[Saved], mons: &[MonitorRow]) {
    unsafe {
        for s in saved {
            let hwnd = hwnd_of(s.hwnd);
            // Gone, or minimized while the screens were off: leave it.
            if !IsWindow(Some(hwnd)).as_bool() || !zonable(hwnd) {
                continue;
            }
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let cur = mons.iter().find(|m| m.handle == hmon).map(|m| m.work);
            if !needs_restore(s.work, cur, mons) {
                continue;
            }
            // Un-maximize first, like move_window, so SetWindowPos sticks.
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_NORMAL);
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                s.rect.l,
                s.rect.t,
                s.rect.w(),
                s.rect.h(),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if s.maximized {
                let _ = ShowWindow(hwnd, SW_MAXIMIZE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::HMONITOR;

    fn mon(work: Rect) -> MonitorRow {
        MonitorRow {
            handle: HMONITOR(std::ptr::null_mut()),
            work,
        }
    }

    #[test]
    fn restore_only_when_the_monitor_is_back_and_the_window_left_it() {
        let primary = Rect {
            l: 0,
            t: 0,
            r: 1000,
            b: 700,
        };
        let secondary = Rect {
            l: 1000,
            t: 0,
            r: 2000,
            b: 700,
        };
        let both = vec![mon(primary), mon(secondary)];

        // Monitor still gone: the window stays on the primary for now.
        assert!(!needs_restore(secondary, Some(primary), &both[..1]));
        // Monitor back, window stranded on the primary: put it back.
        assert!(needs_restore(secondary, Some(primary), &both));
        // No unplug ever happened: leave the window alone.
        assert!(!needs_restore(secondary, Some(secondary), &both));
        // Its monitor is not among the returned ones: nothing to restore to.
        assert!(!needs_restore(secondary, Some(primary), &both[..1]));
    }
}

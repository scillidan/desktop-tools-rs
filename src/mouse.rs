//! Modifier+LMB drag-move (`WH_MOUSE_LL`, Windows only).
//!
//! Holding the configured drag modifier (Win by default) and dragging any
//! point inside a window moves it, even across monitors; a maximized window is
//! restored first. The hook runs on the daemon thread that owns the message
//! loop, swallows the button press/release so the window does not also see a
//! click, and is removed while the tray has hotkeys suspended.

use crate::keys::Mods;
use crate::layout::Rect;
use crate::platform::WinId;
use crate::win32::{focus_window, hwnd_of, id_of, rect_of, zonable};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetWindowRect, IsZoomed, SetWindowPos, SetWindowsHookExW,
    ShowWindow, UnhookWindowsHookEx, WindowFromPoint, GA_ROOT, HC_ACTION, HHOOK, MSLLHOOKSTRUCT,
    SWP_ASYNCWINDOWPOS, SWP_DEFERERASE, SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOSIZE,
    SWP_NOZORDER, SW_RESTORE, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

/// 0 while no hook is installed, the `HHOOK` value otherwise.
static HOOK: AtomicIsize = AtomicIsize::new(0);

/// Drag modifier; `win+alt` until the config says otherwise.
const DEFAULT_MODS: Mods = Mods {
    ctrl: false,
    shift: false,
    alt: true,
    win: true,
};
static MODS: Mutex<Mods> = Mutex::new(DEFAULT_MODS);

pub fn set_modifiers(m: Mods) {
    *MODS.lock().unwrap_or_else(|e| e.into_inner()) = m;
}

fn mods() -> MutexGuard<'static, Mods> {
    MODS.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy)]
struct Drag {
    id: WinId,
    /// Cursor offset inside the window at grab time.
    dx: i32,
    dy: i32,
    /// Frame of a maximized window, consumed by the first move: a plain
    /// modifier+click must not restore it, only an actual drag.
    restore: Option<Rect>,
}

static DRAG: Mutex<Option<Drag>> = Mutex::new(None);

fn drag() -> MutexGuard<'static, Option<Drag>> {
    DRAG.lock().unwrap_or_else(|e| e.into_inner())
}

/// Install the hook on the calling thread, which must pump messages.
pub fn install() -> Result<()> {
    if HOOK.load(Ordering::SeqCst) != 0 {
        return Ok(());
    }
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) }
        .context("cannot install the Win+LMB drag hook")?;
    HOOK.store(hook.0 as isize, Ordering::SeqCst);
    Ok(())
}

pub fn uninstall() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut core::ffi::c_void));
        }
    }
    *drag() = None;
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        match wparam.0 as u32 {
            WM_LBUTTONDOWN => {
                if mods_down() {
                    if let Some(d) = unsafe { start_drag(ms.pt) } {
                        *drag() = Some(d);
                        return LRESULT(1);
                    }
                }
            }
            WM_MOUSEMOVE => {
                // Copy the state out first: a guard in an `if let` scrutinee
                // would still be held inside the body (and deadlock).
                let active = *drag();
                if let Some(d) = active {
                    *drag() = unsafe { move_drag(d, ms.pt) };
                }
            }
            WM_LBUTTONUP => {
                let was_dragging = drag().take().is_some();
                if was_dragging {
                    mask_drag_modifiers();
                    return LRESULT(1);
                }
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// A swallowed click while Win is held would otherwise count as a bare Win
/// tap: releasing Win pops the Start menu. Injecting vk07 (AutoHotkey's
/// MenuMaskKey) while Win is down marks it "used" for the shell.
fn mask_drag_modifiers() {
    if !mods().win {
        return;
    }
    for up in [false, true] {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x07),
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    ..Default::default()
                },
            },
        };
        unsafe {
            let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }
}

/// Focus the window under the cursor and remember the grab offset. A maximized
/// window is only marked for restore here; the first move does that work, so a
/// plain modifier+click leaves it maximized.
unsafe fn start_drag(pt: POINT) -> Option<Drag> {
    let hwnd = unsafe { GetAncestor(WindowFromPoint(pt), GA_ROOT) };
    if !zonable(hwnd) {
        return None;
    }
    let _ = focus_window(hwnd);
    let rect = unsafe { window_rect(hwnd) }?;
    let maximized = unsafe { IsZoomed(hwnd).as_bool() };
    Some(Drag {
        id: id_of(hwnd),
        dx: if maximized { 0 } else { pt.x - rect.l },
        dy: if maximized { 0 } else { pt.y - rect.t },
        restore: maximized.then_some(rect),
    })
}

/// One drag step: restore a maximized window on the first move, then keep the
/// grab point offset.
unsafe fn move_drag(mut d: Drag, pt: POINT) -> Option<Drag> {
    if let Some(maximized) = d.restore {
        let hwnd = hwnd_of(d.id);
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let normal = unsafe { window_rect(hwnd) }?;
        let (l, t) = restored_origin(maximized, (normal.w(), normal.h()), (pt.x, pt.y));
        d.dx = pt.x - l;
        d.dy = pt.y - t;
        d.restore = None;
    }
    unsafe { drag_to(d, pt) };
    Some(d)
}

unsafe fn drag_to(d: Drag, pt: POINT) {
    unsafe {
        // Async + no WM_WINDOWPOSCHANGING: a slow target must not stall the
        // hook and get it silently removed by Windows.
        let _ = SetWindowPos(
            hwnd_of(d.id),
            None,
            pt.x - d.dx,
            pt.y - d.dy,
            0,
            0,
            SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_NOSENDCHANGING
                | SWP_DEFERERASE
                | SWP_ASYNCWINDOWPOS,
        );
    }
}

unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
    let mut rc = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rc) }.ok()?;
    Some(rect_of(rc))
}

/// Top-left for a just-restored window: the grab point keeps the fraction of
/// the frame it had while maximized, so the window does not jump on restore.
fn restored_origin(maximized: Rect, size: (i32, i32), pt: (i32, i32)) -> (i32, i32) {
    let fx = ((pt.0 - maximized.l) as f32 / maximized.w().max(1) as f32).clamp(0.0, 1.0);
    let fy = ((pt.1 - maximized.t) as f32 / maximized.h().max(1) as f32).clamp(0.0, 1.0);
    (
        pt.0 - (fx * size.0 as f32) as i32,
        pt.1 - (fy * size.1 as f32) as i32,
    )
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0 }
}

fn mods_down() -> bool {
    let m = *mods();
    (!m.ctrl || key_down(VK_CONTROL))
        && (!m.shift || key_down(VK_SHIFT))
        && (!m.alt || key_down(VK_MENU))
        && (!m.win || key_down(VK_LWIN) || key_down(VK_RWIN))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_installs_and_uninstalls() {
        install().expect("WH_MOUSE_LL hook");
        assert_ne!(HOOK.load(Ordering::SeqCst), 0);
        uninstall();
        assert_eq!(HOOK.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn restored_origin_keeps_the_grab_fraction() {
        let maximized = Rect {
            l: 0,
            t: 0,
            r: 1920,
            b: 1040,
        };
        // Middle: the restored 800x600 window centers on the grab point.
        assert_eq!(
            restored_origin(maximized, (800, 600), (960, 520)),
            (560, 220)
        );
        // Top-left: the grab stays at the same small fraction.
        assert_eq!(
            restored_origin(maximized, (800, 600), (192, 104)),
            (112, 44)
        );
        // A grab outside the frame (DWM's invisible border) stays on the edge.
        assert_eq!(
            restored_origin(maximized, (800, 600), (-40, 2000)),
            (-40, 1400)
        );
    }
}

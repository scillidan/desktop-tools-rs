//! Suppression hook for Win-containing bindings (`WH_KEYBOARD_LL`, Windows only).
//!
//! Explorer owns the Win+key namespace (taskbar digits, jump lists, ...).
//! `RegisterHotKey` succeeds for those combos, yet the shell still reacts
//! first. This hook swallows our Win-containing bindings before the shell
//! sees the trigger key and posts the binding index to the daemon's message
//! thread. Injected events are ignored, so the daemon's own SendInput
//! (pass-through chords, mask keys) is never eaten.

use crate::config::Binding;
use crate::win32::vk_of_key;
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// Thread message the hook posts to the daemon loop; `wParam` is the index
/// into `cfg.bindings`. A hwnd-less WM_APP+n message cannot collide with any
/// window message (tray, dialogs).
pub const WM_KBDHOOK: u32 = 0x8001;

/// `KBDLLHOOKSTRUCT.flags`: the event came from SendInput/keybd_event.
const LLKHF_INJECTED: u32 = 0x10;
const LLKHF_LOWER_IL_INJECTED: u32 = 0x02;

static HOOK: AtomicIsize = AtomicIsize::new(0);
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// One suppressible combo, pre-resolved to a virtual key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sup {
    vk: u8,
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
    /// Index into `cfg.bindings`, posted back to the daemon loop.
    idx: usize,
}

struct State {
    sup: Vec<Sup>,
    /// Physical key state, tracked because `GetAsyncKeyState` cannot be
    /// trusted from inside the hook that swallows keys.
    down: [bool; 256],
}

static STATE: Mutex<State> = Mutex::new(State {
    sup: Vec::new(),
    down: [false; 256],
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Replace the suppressed set (the Win-containing bindings) with whatever
/// the config currently binds. Called wherever hotkeys are (re)registered.
pub fn set_bindings(bindings: &[Binding]) {
    let sup = bindings
        .iter()
        .enumerate()
        .filter(|(_, b)| b.combo.win)
        .filter_map(|(idx, b)| {
            vk_of_key(b.combo.key).map(|vk| Sup {
                vk: vk.0 as u8,
                ctrl: b.combo.ctrl,
                shift: b.combo.shift,
                alt: b.combo.alt,
                win: b.combo.win,
                idx,
            })
        })
        .collect();
    *state() = State {
        sup,
        down: [false; 256],
    };
}

/// Install the hook on the calling thread, which must pump messages.
pub fn install() -> Result<()> {
    if HOOK.load(Ordering::SeqCst) != 0 {
        return Ok(());
    }
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
        .context("cannot install the Win-combo suppression hook")?;
    HOOK.store(hook.0 as isize, Ordering::SeqCst);
    THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    Ok(())
}

pub fn uninstall() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    THREAD_ID.store(0, Ordering::SeqCst);
    if hook != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut core::ffi::c_void));
        }
    }
    state().sup.clear();
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        if matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
            && kb.flags.0 & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED) == 0
            && kb.vkCode < 0x100
        {
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let mut st = state();
            if let Some(idx) = on_key(&mut st, kb.vkCode as u8, down) {
                drop(st);
                mask_win_release();
                let tid = THREAD_ID.load(Ordering::SeqCst);
                if tid != 0 {
                    unsafe {
                        let _ = PostThreadMessageW(tid, WM_KBDHOOK, WPARAM(idx), LPARAM(0));
                    }
                }
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// State-machine core (unit-tested): record the key, and on a keydown whose
/// exact modifiers match a suppressed combo, report its binding index.
fn on_key(st: &mut State, vk: u8, down: bool) -> Option<usize> {
    st.down[vk as usize] = down;
    if !down {
        return None;
    }
    let (ctrl, shift, alt, win) = mods(&st.down);
    st.sup
        .iter()
        .find(|s| s.vk == vk && s.ctrl == ctrl && s.shift == shift && s.alt == alt && s.win == win)
        .map(|s| s.idx)
}

/// Modifier state from the tracked physical key map (generic + left/right).
fn mods(down: &[bool; 256]) -> (bool, bool, bool, bool) {
    (
        down[0x11] || down[0xA2] || down[0xA3],
        down[0x10] || down[0xA0] || down[0xA1],
        down[0x12] || down[0xA4] || down[0xA5],
        down[0x5B] || down[0x5C],
    )
}

/// Mark Win as "used" so releasing it after a swallowed combo does not pop
/// the Start menu (AutoHotkey's MenuMaskKey trick, vk07).
fn mask_win_release() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn win_alt_sup(vk: u8, idx: usize) -> Sup {
        Sup {
            vk,
            ctrl: false,
            shift: false,
            alt: true,
            win: true,
            idx,
        }
    }

    fn press(st: &mut State, vk: u8) -> Option<usize> {
        on_key(st, vk, true)
    }

    fn release(st: &mut State, vk: u8) {
        let _ = on_key(st, vk, false);
    }

    #[test]
    fn swallows_exact_win_combo_and_stays_silent_afterwards() {
        let mut st = State {
            sup: vec![win_alt_sup(0x31, 7)],
            down: [false; 256],
        };
        assert_eq!(press(&mut st, 0x5B), None); // win down alone: pass
        assert_eq!(press(&mut st, 0x12), None); // alt down: pass
        assert_eq!(press(&mut st, 0x31), Some(7)); // win+alt+1: swallow + fire
        assert_eq!(press(&mut st, 0x31), Some(7)); // auto-repeat: stay swallowed
        release(&mut st, 0x31);
        release(&mut st, 0x12);
        release(&mut st, 0x5B);
        assert_eq!(press(&mut st, 0x31), None); // bare 1 afterwards: pass
    }

    #[test]
    fn extra_modifier_prevents_the_match() {
        let mut st = State {
            sup: vec![win_alt_sup(0x31, 7)],
            down: [false; 256],
        };
        press(&mut st, 0x5B);
        press(&mut st, 0x12);
        press(&mut st, 0x10); // shift joins: no longer an exact match
        assert_eq!(press(&mut st, 0x31), None);
    }

    #[test]
    fn shift_win_combo_is_distinct_from_win_combo() {
        let mut st = State {
            sup: vec![
                Sup {
                    vk: 0xC0,
                    ctrl: false,
                    shift: false,
                    alt: false,
                    win: true,
                    idx: 0,
                },
                Sup {
                    vk: 0xC0,
                    ctrl: false,
                    shift: true,
                    alt: false,
                    win: true,
                    idx: 1,
                },
            ],
            down: [false; 256],
        };
        press(&mut st, 0x5B);
        assert_eq!(press(&mut st, 0xC0), Some(0)); // win+`
        release(&mut st, 0xC0);
        press(&mut st, 0x10);
        assert_eq!(press(&mut st, 0xC0), Some(1)); // win+shift+`
    }

    #[test]
    fn releasing_a_modifier_cancels_the_pending_combo() {
        let mut st = State {
            sup: vec![win_alt_sup(0x31, 7)],
            down: [false; 256],
        };
        press(&mut st, 0x5B);
        press(&mut st, 0x12);
        release(&mut st, 0x12);
        assert_eq!(press(&mut st, 0x31), None); // win only now
    }
}

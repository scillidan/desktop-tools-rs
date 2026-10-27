//! Put Save/Open dialogs into Explorer's current folder (Windows only).
//!
//! A 250 ms timer watches the foreground window: an Explorer window refreshes
//! the remembered folder, a file dialog (class `#32770` with an `Edit`,
//! `ToolbarWindow32` and a shell view) is navigated there through its address
//! bar (`Ctrl+L`, path, Enter). Only the most recently active Explorer window
//! is used, like QuickSwitch's auto mode; no menu, no per-dialog lists.

use crate::vdesk::com_init;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use windows::core::{w, Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_DELETE, VK_RETURN,
};
use windows::Win32::UI::Shell::{IShellWindows, IWebBrowser2, PathCreateFromUrlW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, SendMessageTimeoutW, SetForegroundWindow,
    SMTO_ABORTIFHUNG,
};

const CLSID_SHELL_WINDOWS: GUID = GUID::from_u128(0x9BA05972_F6A8_11CF_A442_00A0C90A8F39);

struct State {
    /// Folder of the most recently active file manager.
    folder: Option<PathBuf>,
    /// Dialog already handled, as a raw HWND, so focus changes inside it do
    /// not navigate again.
    dialog: isize,
    /// Auto attempts spent on that dialog; a dialog that is still loading can
    /// fail once and then succeed.
    tries: u8,
}

static STATE: Mutex<State> = Mutex::new(State {
    folder: None,
    dialog: 0,
    tries: 0,
});
static FILLING: AtomicBool = AtomicBool::new(false);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called from the daemon's 250 ms timer. `auto` is the `quickSwitch`
/// setting: the folder is always remembered so the manual `fileDialog` key
/// works, but dialogs are only navigated automatically when it is on.
pub fn tick(auto: bool) {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        let class = class_name(fg);
        if let Some(folder) = file_manager_folder(fg, &class) {
            let mut st = state();
            st.dialog = 0;
            st.tries = 0;
            st.folder = Some(folder);
            return;
        }
        if class != "#32770" || !is_file_dialog(fg) {
            let mut st = state();
            st.dialog = 0;
            st.tries = 0;
            return;
        }
        if !auto || FILLING.load(Ordering::Acquire) {
            return;
        }

        let folder = {
            let mut st = state();
            if st.dialog != fg.0 as isize {
                st.dialog = fg.0 as isize;
                st.tries = 0;
            }
            if st.tries >= 4 {
                return;
            }
            st.tries += 1;
            st.folder.clone()
        };
        let Some(folder) = folder else {
            return;
        };
        spawn_fill(fg, folder, fill_dialog);
    }
}

/// Auto-fill must not block the daemon thread: its low-level keyboard/mouse
/// hooks would otherwise stall input for every application on the desktop.
fn spawn_fill<F>(dialog: HWND, folder: PathBuf, fill: F)
where
    F: FnOnce(HWND, &Path) -> Result<()> + Send + 'static,
{
    if FILLING.swap(true, Ordering::AcqRel) {
        return;
    }
    // HWND is !Send only because it wraps a raw pointer; the handle is valid
    // on any thread, so carry its integer value into the worker.
    let raw = dialog.0 as isize;
    std::thread::spawn(move || {
        let dialog = HWND(raw as *mut core::ffi::c_void);
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FILLING.store(false, Ordering::Release);
            }
        }
        let _reset = Reset;
        match fill(dialog, &folder) {
            Ok(()) => {
                let mut st = state();
                if st.dialog == dialog.0 as isize {
                    st.tries = 4;
                }
            }
            Err(e) => eprintln!("warn: dialog folder: {e:#}"),
        }
    });
}

/// Manual trigger (`fileDialog`): apply the remembered folder to the dialog in
/// front, even if auto mode already gave up on it.
pub fn fill_foreground() -> Result<()> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() || !is_file_dialog(fg) {
            bail!("no file dialog is in front");
        }
        if FILLING.load(Ordering::Acquire) {
            bail!("a dialog folder is already being applied");
        }
        let folder = state()
            .folder
            .clone()
            .context("no file-manager folder remembered yet")?;
        fill_dialog(fg, &folder)
    }
}

/// Whether the window in front is a file dialog right now.
pub fn foreground_is_dialog() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        !fg.0.is_null() && class_name(fg) == "#32770" && is_file_dialog(fg)
    }
}

/// Folder of the foreground window when it is a supported file manager.
fn file_manager_folder(hwnd: HWND, class: &str) -> Option<PathBuf> {
    match class {
        "CabinetWClass" => unsafe { explorer_folder(hwnd).ok() },
        "TTOTAL_CMD" => double_commander_folder(&window_title(hwnd)),
        // yazi and other TUIs can put their cwd in the terminal title.
        "org.wezfurlong.wezterm"
        | "ConsoleWindowClass"
        | "CASCADIA_HOSTING_WINDOW_CLASS"
        | "mintty" => path_title(&window_title(hwnd)),
        _ => None,
    }
}

/// Double Commander titles the window `name (C:\path\) - Double Commander 1.2`.
fn double_commander_folder(title: &str) -> Option<PathBuf> {
    let stem = title.split(" - Double Commander").next()?;
    let start = stem.rfind('(')? + 1;
    let end = stem.rfind(')')?;
    let path = PathBuf::from(stem.get(start..end)?.trim());
    path.is_dir().then_some(path)
}

/// A terminal title that is itself an existing directory (yazi's default).
fn path_title(title: &str) -> Option<PathBuf> {
    let path = PathBuf::from(title.trim());
    if path.is_absolute() && path.is_dir() {
        Some(path)
    } else {
        None
    }
}

fn window_title(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    let mut buf = [0u16; 1024];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Rough file-dialog check from QuickSwitch: a message box has none of these.
fn is_file_dialog(dialog: HWND) -> bool {
    let mut found = [false; 4];
    unsafe {
        let _ = EnumChildWindows(
            Some(dialog),
            Some(enum_child),
            LPARAM(found.as_mut_ptr() as isize),
        );
    }
    found[0] && found[1] && (found[2] || found[3])
}

unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut [bool; 4]) };
    match class_name(hwnd).as_str() {
        "Edit" => found[0] = true,
        "ToolbarWindow32" => found[1] = true,
        "DirectUIHWND" => found[2] = true,
        "SysListView32" => found[3] = true,
        _ => {}
    }
    BOOL(1)
}

/// Folder of the Explorer window with this HWND, via `ShellWindows`.
unsafe fn explorer_folder(hwnd: HWND) -> Result<PathBuf> {
    unsafe {
        com_init()?;
        let windows: IShellWindows = CoCreateInstance(&CLSID_SHELL_WINDOWS, None, CLSCTX_ALL)
            .context("cannot create the ShellWindows object")?;
        let count = windows.Count().context("cannot count shell windows")?;
        for i in 0..count {
            let mut index = VARIANT::default();
            {
                let var = &mut *index.Anonymous.Anonymous;
                var.vt = VT_I4;
                var.Anonymous.lVal = i;
            }
            let Ok(dispatch) = windows.Item(&index) else {
                continue;
            };
            let Ok(browser) = dispatch.cast::<IWebBrowser2>() else {
                continue;
            };
            let Ok(found) = browser.HWND() else {
                continue;
            };
            if found.0 != hwnd.0 as isize {
                continue;
            }
            let url = browser
                .LocationURL()
                .context("cannot read the location URL")?;
            return url_to_path(&url.to_string());
        }
        bail!("Explorer window not found in ShellWindows")
    }
}

fn url_to_path(url: &str) -> Result<PathBuf> {
    let wide: Vec<u16> = url.encode_utf16().chain([0]).collect();
    // Long-path sized buffer: the two-call sizing form of PathCreateFromUrlW
    // does not report the needed length on this Windows version.
    let mut buf = vec![0u16; 32_768];
    let mut written = buf.len() as u32;
    unsafe {
        PathCreateFromUrlW(
            PCWSTR(wide.as_ptr()),
            PWSTR(buf.as_mut_ptr()),
            &mut written,
            0,
        )?
    };
    let text = String::from_utf16_lossy(&buf[..(written as usize).min(buf.len())]);
    let text = text.trim_end_matches('\0');
    if text.is_empty() {
        bail!("cannot turn `{url}` into a folder path");
    }
    Ok(PathBuf::from(text))
}

/// Navigate the dialog through its File name box: focus it, type the folder
/// with a trailing `\`, press Enter, then put the old name back. Typing (not
/// SetWindowText) is what makes the dialog see a normal edit and commit it.
fn fill_dialog(dialog: HWND, folder: &Path) -> Result<()> {
    unsafe {
        wait_modifiers_released();
        let _ = SetForegroundWindow(dialog);
        std::thread::sleep(Duration::from_millis(40));
        if GetForegroundWindow() != dialog {
            // Never type into whatever window happens to be in front.
            bail!("the dialog is not in front");
        }
        let Some(edit) = filename_edit(dialog) else {
            bail!("the dialog has no file name box");
        };
        let old = control_text(edit);
        let mut target = folder.to_string_lossy().trim_end_matches('\\').to_string();
        target.push('\\');

        for _ in 0..2 {
            focus_control(dialog, edit);
            std::thread::sleep(Duration::from_millis(60));
            select_all(edit);
            if !type_text(&target) {
                bail!("cannot type the folder path into the dialog");
            }
            std::thread::sleep(Duration::from_millis(100));
            if control_text(edit) == target {
                if !send(&[key(VK_RETURN, false), key(VK_RETURN, true)]) {
                    bail!("cannot press Enter in the dialog");
                }
                std::thread::sleep(Duration::from_millis(150));
                // Put the suggested name back (an empty one clears the box).
                focus_control(dialog, edit);
                std::thread::sleep(Duration::from_millis(30));
                select_all(edit);
                if old.is_empty() {
                    let _ = send(&[key(VK_DELETE, false), key(VK_DELETE, true)]);
                } else {
                    let _ = type_text(&old);
                }
                return Ok(());
            }
        }
        bail!("could not put the folder path into the file name box")
    }
}

/// A hung dialog must never hold the daemon (or its input hooks) indefinitely.
unsafe fn send_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { SendMessageTimeoutW(hwnd, msg, wparam, lparam, SMTO_ABORTIFHUNG, 250, None) }
}

/// Focus a control inside a dialog of another process.
unsafe fn focus_control(dialog: HWND, control: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::WM_NEXTDLGCTL;
    unsafe {
        let _ = send_message(dialog, WM_NEXTDLGCTL, WPARAM(control.0 as usize), LPARAM(1));
    }
}

fn select_all(edit: HWND) {
    const EM_SETSEL: u32 = 0x00B1;
    unsafe {
        let _ = send_message(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
    }
}

fn control_text(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::WM_GETTEXT;
    let mut buf = [0u16; 512];
    let len = unsafe {
        send_message(
            hwnd,
            WM_GETTEXT,
            WPARAM(buf.len()),
            LPARAM(buf.as_mut_ptr() as isize),
        )
    };
    String::from_utf16_lossy(&buf[..(len.0 as usize).min(buf.len())])
}

/// The File name box of a common dialog.
unsafe fn filename_edit(dialog: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
    unsafe {
        let combo = FindWindowExW(Some(dialog), None, w!("ComboBoxEx32"), None).ok()?;
        let inner = FindWindowExW(Some(combo), None, w!("ComboBox"), None).ok()?;
        FindWindowExW(Some(inner), None, w!("Edit"), None).ok()
    }
}

fn wait_modifiers_released() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_MENU, VK_SHIFT};
    for _ in 0..20 {
        let down = unsafe {
            GetAsyncKeyState(VK_CONTROL.0 as i32) < 0
                || GetAsyncKeyState(VK_MENU.0 as i32) < 0
                || GetAsyncKeyState(VK_SHIFT.0 as i32) < 0
        };
        if !down {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Type `text` as Unicode key events, so the target control sees normal input
/// and updates its own state (a plain WM_SETTEXT does not).
fn type_text(text: &str) -> bool {
    let mut inputs = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        inputs.push(unicode_key(unit, false));
        inputs.push(unicode_key(unit, true));
    }
    send(&inputs)
}

fn unicode_key(unit: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: if up {
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                } else {
                    KEYEVENTF_UNICODE
                },
                ..Default::default()
            },
        },
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
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
    }
}

fn send(inputs: &[INPUT]) -> bool {
    unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cabinets() -> Vec<HWND> {
        use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
            if class_name(hwnd) == "CabinetWClass" {
                out.push(hwnd);
            }
            BOOL(1)
        }
        let mut out = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut out as *mut _ as isize));
        }
        out
    }

    #[test]
    fn auto_fill_does_not_block_the_daemon() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        spawn_fill(
            HWND(std::ptr::dangling_mut::<core::ffi::c_void>()),
            PathBuf::from("folder"),
            move |_, _| {
                std::thread::sleep(Duration::from_millis(300));
                done_tx.send(()).unwrap();
                Ok(())
            },
        );
        assert!(started.elapsed() < Duration::from_millis(100));
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            url_to_path("file:///C:/Users/User/Share").unwrap(),
            PathBuf::from(r"C:\Users\User\Share")
        );
        assert_eq!(
            url_to_path("file:///C:/Program%20Files").unwrap(),
            PathBuf::from(r"C:\Program Files")
        );
        assert!(url_to_path("not a url").is_err());
    }

    #[test]
    fn parses_file_manager_titles() {
        assert_eq!(
            double_commander_folder(r"registry (C:\Windows\) - Double Commander 1.2.9~395"),
            Some(PathBuf::from(r"C:\Windows\"))
        );
        assert_eq!(
            double_commander_folder(r"C:\Windows - Double Commander 1.0"),
            None
        );
        assert_eq!(
            path_title(r"C:\Windows"),
            Some(PathBuf::from(r"C:\Windows"))
        );
        assert_eq!(path_title("[1/3] OC | Roadmap item 4"), None);
    }

    #[test]
    #[ignore = "opens and closes a real Explorer window"]
    #[allow(clippy::zombie_processes)] // Explorer keeps running after WM_CLOSE.
    fn reads_an_explorer_window_folder() {
        use windows::Win32::Foundation::WPARAM;
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

        let before = cabinets();
        std::process::Command::new("explorer.exe")
            .arg(r"C:\Windows")
            .spawn()
            .expect("start Explorer");
        let mut found = None;
        for _ in 0..40 {
            found = cabinets()
                .into_iter()
                .find(|h| !before.iter().any(|old| old.0 == h.0));
            if found.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let hwnd = found.expect("the new Explorer window");
        let mut folder = None;
        for _ in 0..30 {
            match unsafe { explorer_folder(hwnd) } {
                Ok(f) => {
                    folder = Some(f);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        assert_eq!(
            folder.expect("read the folder"),
            PathBuf::from(r"C:\Windows")
        );
    }
}

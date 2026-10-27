//! Windows tray icon + menu and HKCU Run autostart.
//!
//! The icon lives on the daemon's main thread: its hidden window must be
//! pumped by the same `GetMessageW` loop that dispatches `WM_HOTKEY`.
//! Menu clicks arrive through `MenuEvent`'s global channel, so the loop only
//! needs a non-blocking `try_recv` after each message (keytools-rs pattern).

use anyhow::{Context, Result};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use tray_icon::menu::{
    CheckMenuItemBuilder, Menu, MenuEvent, MenuId, MenuItemBuilder, PredefinedMenuItem,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::core::HSTRING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "desktop-tools-rs";
/// Older autostart value names, migrated once by `migrate_autostart`.
const OLD_APP_NAMES: &[&str] = &["window-tools-rs", "windowtools-rs", "rectangle-rs"];

#[derive(Clone, Copy)]
pub enum Cmd {
    ToggleAutostart,
    Reload,
    Edit,
    UnpinAll,
    ToggleSuspend,
    Quit,
}

/// What the icon shows. `Reload*` are transient; the daemon reverts them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Normal,
    Suspended,
    ReloadOk,
    ReloadFail,
}

pub struct Tray {
    icon: TrayIcon,
    autostart: bool,
    suspended: bool,
    state: State,
    items: Vec<(MenuId, Cmd)>,
}

impl Tray {
    pub fn new() -> Result<Tray> {
        migrate_autostart();
        let autostart = autostart_enabled();
        let icon = TrayIconBuilder::new()
            .with_tooltip(APP_NAME)
            .with_icon(icon(State::Normal, false)?)
            .build()
            .context("cannot create tray icon")?;
        let mut tray = Tray {
            icon,
            autostart,
            suspended: false,
            state: State::Normal,
            items: Vec::new(),
        };
        tray.rebuild();
        Ok(tray)
    }

    /// Tooltip text; truncated to the 128-char NOTIFYICONDATA limit.
    pub fn set_status(&self, text: &str) {
        let text: String = text.chars().take(120).collect();
        let _ = self.icon.set_tooltip(Some(text));
    }

    /// Swap the icon. A failed swap only loses the visual cue, so it is not
    /// reported to the caller.
    pub fn set_state(&mut self, state: State) {
        if state == self.state {
            return;
        }
        self.state = state;
        if let Ok(img) = icon(state, self.suspended) {
            let _ = self.icon.set_icon(Some(img));
        }
    }

    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        self.rebuild();
        self.set_state(if suspended {
            State::Suspended
        } else {
            State::Normal
        });
    }

    pub fn toggle_autostart(&mut self) -> Result<()> {
        let on = !self.autostart;
        set_autostart(on)?;
        self.autostart = on;
        self.rebuild();
        Ok(())
    }

    /// Next menu click, if any. Never blocks, so hotkeys stay responsive.
    pub fn poll(&self) -> Option<Cmd> {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(&(_, cmd)) = self.items.iter().find(|(id, _)| *id == event.id) {
                return Some(cmd);
            }
        }
        None
    }

    fn rebuild(&mut self) {
        let menu = Menu::new();
        let mut items = Vec::new();

        let auto = CheckMenuItemBuilder::new()
            .text("Start with Windows")
            .checked(self.autostart)
            .enabled(true) // muda defaults to disabled, which greys out every row
            .build();
        let _ = menu.append(&auto);
        items.push((auto.id().clone(), Cmd::ToggleAutostart));

        item(&menu, &mut items, "Reload config", Cmd::Reload);
        item(&menu, &mut items, "Edit config", Cmd::Edit);
        item(&menu, &mut items, "Unpin all", Cmd::UnpinAll);
        item(
            &menu,
            &mut items,
            if self.suspended {
                "Resume hotkeys"
            } else {
                "Suspend hotkeys"
            },
            Cmd::ToggleSuspend,
        );
        let _ = menu.append(&PredefinedMenuItem::separator());
        item(&menu, &mut items, "Quit", Cmd::Quit);

        self.items = items;
        self.icon.set_menu(Some(Box::new(menu)));
    }
}

fn item(menu: &Menu, items: &mut Vec<(MenuId, Cmd)>, label: &str, cmd: Cmd) {
    let entry = MenuItemBuilder::new().text(label).enabled(true).build();
    items.push((entry.id().clone(), cmd));
    let _ = menu.append(&entry);
}

/// Logical icon size. 32 px is the tray-icon default; the shell downscales it
/// to 16/20/24 px, so it is drawn supersampled (see `SS`).
const N: i32 = 32;

/// An abstract window: a small hollow title bar above a filled blue body.
/// The 22x22 glyph is large enough for a 16 px tray icon without crowding it.
/// Grey means suspended; the reload badge remains yellow/red.
fn icon(state: State, suspended: bool) -> Result<Icon> {
    let px = draw(state, suspended, N);
    Icon::from_rgba(px, N as u32, N as u32).context("invalid tray icon pixels")
}

/// The same glyph at any square size (tray icon, exe icon). Coordinates are
/// authored at 32 px and scaled per size.
fn draw(state: State, suspended: bool, n: i32) -> Vec<u8> {
    let mut c = Canvas::new(n);
    let ink = if suspended || state == State::Suspended {
        GREY
    } else {
        BLUE
    };
    c.rect(4.0, 6.0, 28.0, 26.0, ink);
    c.rect(7.0, 9.0, 25.0, 23.0, CLEAR);
    c.rect(7.0, 9.0, 16.0, 16.0, ink);
    match state {
        State::ReloadOk => c.badge(YELLOW),
        State::ReloadFail => c.badge(RED),
        _ => {}
    }
    c.downsample()
}

const BLUE: [u8; 4] = [37, 99, 235, 255];
const GREY: [u8; 4] = [107, 114, 128, 255];
const YELLOW: [u8; 4] = [245, 179, 1, 255];
const RED: [u8; 4] = [220, 38, 38, 255];
const RING: [u8; 4] = [31, 41, 55, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Supersampling factor: draw at 128x128, box-filter down to 32x32, so the
/// circle badge and the downscaled 16/20/24 px icons stay clean.
const SS: i32 = 4;

/// The glyph is authored at 32 px; other sizes scale the coordinates.
const AUTHORED: i32 = 32;

struct Canvas {
    n: i32,
    px: Vec<[u8; 4]>,
}

impl Canvas {
    fn new(n: i32) -> Self {
        Canvas {
            n,
            px: vec![[0; 4]; (n * SS * n * SS) as usize],
        }
    }

    fn set(&mut self, x: i32, y: i32, color: [u8; 4]) {
        let w = self.n * SS;
        if (0..w).contains(&x) && (0..w).contains(&y) {
            self.px[(y * w + x) as usize] = color;
        }
    }

    /// Authored (32-px) coordinates to this canvas's pixels.
    fn scale(&self) -> f32 {
        SS as f32 * self.n as f32 / AUTHORED as f32
    }

    /// Axis-aligned fill in authored coordinates.
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
        let s = self.scale();
        for y in (y0 * s).round() as i32..(y1 * s).round() as i32 {
            for x in (x0 * s).round() as i32..(x1 * s).round() as i32 {
                self.set(x, y, color);
            }
        }
    }

    /// Filled circle (the reload badge), in authored coordinates.
    fn disc(&mut self, cx: f32, cy: f32, r: f32, color: [u8; 4]) {
        let s = self.scale();
        for y in ((cy - r) * s).floor() as i32..((cy + r) * s).ceil() as i32 {
            for x in ((cx - r) * s).floor() as i32..((cx + r) * s).ceil() as i32 {
                let px = (x as f32 + 0.5) / s;
                let py = (y as f32 + 0.5) / s;
                if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                    self.set(x, y, color);
                }
            }
        }
    }

    /// Bottom-right reload dot. The dark ring keeps yellow visible on a light
    /// taskbar; on a dark one the dot itself carries the contrast.
    fn badge(&mut self, color: [u8; 4]) {
        self.disc(24.0, 24.0, 5.4, RING);
        self.disc(24.0, 24.0, 4.0, color);
    }

    /// Premultiplied box filter, so partly covered edges do not get a dark
    /// fringe against the transparent background.
    fn downsample(self) -> Vec<u8> {
        let n = self.n;
        let samples = (SS * SS) as u32;
        let mut out = vec![0u8; (n * n * 4) as usize];
        for y in 0..n {
            for x in 0..n {
                let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
                for dy in 0..SS {
                    for dx in 0..SS {
                        let p = self.px[((y * SS + dy) * n * SS + x * SS + dx) as usize];
                        let al = p[3] as u32;
                        r += p[0] as u32 * al;
                        g += p[1] as u32 * al;
                        b += p[2] as u32 * al;
                        a += al;
                    }
                }
                let i = ((y * n + x) * 4) as usize;
                let a = a.max(1);
                out[i] = (r / a) as u8;
                out[i + 1] = (g / a) as u8;
                out[i + 2] = (b / a) as u8;
                out[i + 3] = (a / samples) as u8;
            }
        }
        out
    }
}

/// The tray glyph as a Windows `.ico`: 48/32/16 px BMP-DIB entries, bottom-up
/// straight-alpha BGRA with an empty AND mask (alpha does the transparency).
/// Regenerate `assets/desktop-tools.ico` with the ignored `write_icon_asset`
/// test; `build.rs` embeds that file into the exe.
#[allow(dead_code)] // only the regeneration test reads this in-tree
pub fn ico_bytes() -> Vec<u8> {
    const SIZES: [u32; 3] = [48, 32, 16];
    let images: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&n| draw(State::Normal, false, n as i32))
        .collect();
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0, SIZES.len() as u8, 0]);
    let mut offset = (6 + 16 * SIZES.len()) as u32;
    for &n in &SIZES {
        let mask_row = n.div_ceil(32) * 4;
        let size = 40 + n * n * 4 + mask_row * n;
        ico.extend_from_slice(&[n as u8, n as u8, 0, 0]); // width, height, colors, reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&size.to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += size;
    }
    for (i, &n) in SIZES.iter().enumerate() {
        let px = &images[i];
        let mask_row = (n.div_ceil(32) * 4) as usize;
        ico.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        ico.extend_from_slice(&n.to_le_bytes());
        ico.extend_from_slice(&(n * 2).to_le_bytes()); // DIB height: image + AND mask
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&[0; 24]); // BI_RGB: no compression, no palette
        for y in (0..n).rev() {
            for x in 0..n {
                let at = ((y * n + x) * 4) as usize;
                ico.extend_from_slice(&[px[at + 2], px[at + 1], px[at], px[at + 3]]);
            }
        }
        ico.resize(ico.len() + mask_row * n as usize, 0); // empty AND mask
    }
    ico
}

/// The old builds registered `HKCU\Run\window-tools-rs` / `windowtools-rs` /
/// `rectangle-rs`; move that setting to `desktop-tools-rs` with the current
/// exe path so autostart survives the rename.
fn migrate_autostart() {
    for old in OLD_APP_NAMES {
        if run_value(old).is_some() {
            if run_value(APP_NAME).is_none() {
                if let Err(e) = set_autostart(true) {
                    eprintln!("warn: cannot migrate autostart: {e:#}");
                }
            }
            delete_run_value(old);
        }
    }
    // Moving the folder leaves the Run value pointing at the old exe path.
    if autostart_enabled() {
        if let Ok(exe) = std::env::current_exe() {
            let want = format!("\"{}\"", exe.display());
            if run_value(APP_NAME).as_deref() != Some(want.as_str()) {
                if let Err(e) = set_autostart(true) {
                    eprintln!("warn: cannot refresh autostart path: {e:#}");
                }
            }
        }
    }
}

fn run_value(name: &str) -> Option<String> {
    let subkey = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(name);
    let mut len = 0u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut len),
        )
    };
    if r != ERROR_SUCCESS || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    let mut bytes = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut bytes),
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    Some(
        String::from_utf16_lossy(&buf)
            .trim_end_matches('\0')
            .to_string(),
    )
}

fn delete_run_value(name: &str) {
    let subkey = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(name);
    let mut key = windows::Win32::System::Registry::HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_SET_VALUE, &mut key) };
    if r == ERROR_SUCCESS {
        unsafe {
            let _ = RegDeleteValueW(key, &value);
            let _ = windows::Win32::System::Registry::RegCloseKey(key);
        }
    }
}

fn autostart_enabled() -> bool {
    run_value(APP_NAME).is_some()
}

fn set_autostart(on: bool) -> Result<()> {
    let subkey = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(APP_NAME);
    let mut key = windows::Win32::System::Registry::HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_SET_VALUE, &mut key) };
    anyhow::ensure!(
        r == ERROR_SUCCESS,
        "cannot open HKCU\\{RUN_KEY} (error {})",
        r.0
    );
    let result = unsafe {
        if on {
            // Quoted so a path with spaces survives Run's command-line parse.
            let exe = std::env::current_exe().context("cannot locate own executable")?;
            let cmd = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            RegSetValueExW(key, &value, Some(0), REG_SZ, Some(bytes))
        } else {
            RegDeleteValueW(key, &value)
        }
    };
    let _ = unsafe { windows::Win32::System::Registry::RegCloseKey(key) };
    anyhow::ensure!(
        result == ERROR_SUCCESS,
        "cannot {} the autostart value (error {})",
        if on { "write" } else { "remove" },
        result.0
    );
    Ok(())
}

/// Open desktop-tools.toml: `EDITOR` / `VISUAL` when set, else the default handler.
pub fn edit_config(path: &Path) -> Result<()> {
    if let Some(editor) = std::env::var_os("EDITOR").or_else(|| std::env::var_os("VISUAL")) {
        if !editor.is_empty() {
            spawn_editor(&editor.to_string_lossy(), path)?;
            return Ok(());
        }
    }
    let file = HSTRING::from(path.to_string_lossy().as_ref());
    let r = unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            &file,
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    anyhow::ensure!(r.0 as isize > 32, "no handler for .toml files");
    Ok(())
}

fn spawn_editor(editor: &str, path: &Path) -> Result<()> {
    editor_command(editor, path)
        .spawn()
        .with_context(|| format!("cannot launch EDITOR={editor}"))?;
    Ok(())
}

/// Editors go through `cmd /S /C` so `.cmd` shims (VS Code's `code`) resolve
/// and `EDITOR` values with arguments keep working. `/S` strips the first and
/// last quote, so the tail needs its own outer pair; without it cmd would eat
/// the quotes around a path with spaces.
fn editor_command(editor: &str, path: &Path) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.args(["/S", "/C"])
        .raw_arg(format!("\"{editor} \"{}\"\"", path.display()))
        .creation_flags(CREATE_NO_WINDOW.0);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_draws_a_distinct_icon() {
        let normal = draw(State::Normal, false, N);
        assert_eq!(normal.len(), (N * N * 4) as usize);
        let suspended = draw(State::Suspended, true, N);
        let ok = draw(State::ReloadOk, false, N);
        let fail = draw(State::ReloadFail, false, N);
        assert_ne!(normal, suspended);
        assert_ne!(normal, ok);
        assert_ne!(normal, fail);
        assert_ne!(ok, fail);
    }

    #[test]
    fn abstract_window_has_a_filled_quarter_and_stays_grey_when_suspended() {
        let normal = draw(State::Normal, false, N);
        let alpha = |px: &[u8], x: i32, y: i32| px[((y * N + x) * 4 + 3) as usize];
        assert_eq!(alpha(&normal, 0, 0), 0, "margin is transparent");
        assert_eq!(alpha(&normal, 4, 6), 255, "outline is opaque");
        assert_eq!(alpha(&normal, 8, 10), 255, "upper-left quarter is filled");
        assert_eq!(alpha(&normal, 20, 20), 0, "lower-right window stays clear");
        assert_eq!(alpha(&normal, 16, 27), 0, "there is no monitor stand");

        let grey = draw(State::ReloadOk, true, N);
        let at = ((6 * N + 4) * 4) as usize;
        assert_eq!(&grey[at..at + 3], &GREY[..3], "suspended stays grey");
    }

    #[test]
    fn ico_has_three_sizes_and_a_coherent_directory() {
        let ico = ico_bytes();
        assert_eq!(&ico[0..4], &[0, 0, 1, 0], "reserved 0, type 1 = icon");
        assert_eq!(ico[4], 3);
        let mut expected_offset = (6 + 16 * 3) as u32;
        for (i, size) in [48u8, 32, 16].iter().enumerate() {
            let at = 6 + i * 16;
            assert_eq!(ico[at], *size);
            assert_eq!(ico[at + 2], 0, "colors");
            assert_eq!(ico[at + 3], 0, "reserved");
            assert_eq!(&ico[at + 4..at + 6], &1u16.to_le_bytes(), "planes");
            assert_eq!(&ico[at + 6..at + 8], &32u16.to_le_bytes(), "bpp");
            let size = u32::from_le_bytes(ico[at + 8..at + 12].try_into().unwrap());
            let offset = u32::from_le_bytes(ico[at + 12..at + 16].try_into().unwrap());
            assert_eq!(
                offset, expected_offset,
                "entry {i} starts where the previous ended"
            );
            expected_offset += size;
        }
        assert_eq!(
            expected_offset,
            ico.len() as u32,
            "the last entry ends at the file end"
        );
    }

    #[test]
    #[ignore = "regenerates assets/desktop-tools.ico; run explicitly after editing the glyph"]
    fn write_icon_asset() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("desktop-tools.ico"), ico_bytes()).unwrap();
    }

    /// End-to-end check of the cmd quoting: an editor path *and* a config path
    /// that both contain spaces must reach the editor as one argument each.
    #[test]
    fn editor_command_quotes_spaced_paths() {
        let dir =
            std::env::temp_dir().join(format!("desktop-tools-rs-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("args.txt");
        let script = dir.join("fake editor.cmd");
        std::fs::write(&script, format!("@echo %* > \"{}\"\r\n", log.display())).unwrap();
        let target = dir.join("desktop-tools.toml");
        std::fs::write(&target, "").unwrap();

        let editor = format!("\"{}\"", script.display());
        let mut child = editor_command(&editor, &target).spawn().unwrap();
        child.wait().unwrap();

        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains(&target.display().to_string()), "{logged}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

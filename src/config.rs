//! TOML config: flat `combo = "action"` table plus a few scalar settings
//! (`gap`, `dragModifier`), first-run create next to the exe (keymap-rs /
//! keytools-rs convention).

use crate::keys::{Combo, Mods};
use crate::layout::SizeSpec;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

/// Everything a hotkey can do. Names match RectangleWin(Plus) actions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    MoveToLeft,
    MoveToRight,
    MoveToTop,
    MoveToBottom,
    MoveToTopLeft,
    MoveToTopRight,
    MoveToBottomLeft,
    MoveToBottomRight,
    MoveToCenter,
    Maximize,
    NextDisplay,
    PrevDisplay,
    ToggleAlwaysOnTop,
    ToggleAlwaysOnBottom,
    Minimize,
    UnpinAll,
    NextWindow,
    PrevWindow,
    SizerMenu,
    Undo,
    Quit,
    /// Put the file dialog in front into the last file manager's folder.
    FileDialog,
    /// Switch to the Nth virtual desktop (1-9).
    SwitchToDesktop(u8),
    /// Move the foreground window to the Nth virtual desktop (1-9).
    MoveToDesktop(u8),
    /// Power all monitors off/on; press again to wake them (Windows).
    ToggleScreen,
    /// Swallow the combo without doing anything — for explicitly disabling
    /// shell shortcuts you never use (e.g. `"win+=" = "suppress"` keeps
    /// Magnifier from opening). Win combos only; Win+L and
    /// Win+Ctrl+Shift+B are owned by winlogon/dwm and cannot be hooked.
    Suppress,
    /// Switch to the next/prev virtual desktop, wrapping around.
    NextDesktop,
    PrevDesktop,
}

impl FromStr for Action {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Action> {
        Ok(match s {
            "moveToLeft" => Action::MoveToLeft,
            "moveToRight" => Action::MoveToRight,
            "moveToTop" => Action::MoveToTop,
            "moveToBottom" => Action::MoveToBottom,
            "moveToTopLeft" => Action::MoveToTopLeft,
            "moveToTopRight" => Action::MoveToTopRight,
            "moveToBottomLeft" => Action::MoveToBottomLeft,
            "moveToBottomRight" => Action::MoveToBottomRight,
            "moveToCenter" => Action::MoveToCenter,
            "maximize" => Action::Maximize,
            "nextDisplay" => Action::NextDisplay,
            "prevDisplay" => Action::PrevDisplay,
            "toggleAlwaysOnTop" => Action::ToggleAlwaysOnTop,
            "toggleAlwaysOnBottom" => Action::ToggleAlwaysOnBottom,
            "minimize" => Action::Minimize,
            "unpinAll" => Action::UnpinAll,
            "nextWindow" => Action::NextWindow,
            "prevWindow" => Action::PrevWindow,
            "sizerMenu" => Action::SizerMenu,
            "undo" => Action::Undo,
            "fileDialog" => Action::FileDialog,
            "toggleScreen" => Action::ToggleScreen,
            "suppress" => Action::Suppress,
            "nextDesktop" => Action::NextDesktop,
            "prevDesktop" => Action::PrevDesktop,
            "quit" => Action::Quit,
            other => {
                if let Some(n) = numbered(other, "switchToDesktop") {
                    return Ok(Action::SwitchToDesktop(n));
                }
                if let Some(n) = numbered(other, "moveToDesktop") {
                    return Ok(Action::MoveToDesktop(n));
                }
                anyhow::bail!(
                    "unknown action `{other}` (known: {})",
                    Action::names().join(", ")
                )
            }
        })
    }
}

/// `switchToDesktop3` -> Some(3); only 1..=9, so a typo stays an error.
fn numbered(s: &str, prefix: &str) -> Option<u8> {
    let n = s.strip_prefix(prefix)?.parse::<u8>().ok()?;
    (1..=9).contains(&n).then_some(n)
}

impl Action {
    pub fn names() -> &'static [&'static str] {
        &[
            "moveToLeft",
            "moveToRight",
            "moveToTop",
            "moveToBottom",
            "moveToTopLeft",
            "moveToTopRight",
            "moveToBottomLeft",
            "moveToBottomRight",
            "moveToCenter",
            "maximize",
            "nextDisplay",
            "prevDisplay",
            "toggleAlwaysOnTop",
            "toggleAlwaysOnBottom",
            "minimize",
            "unpinAll",
            "nextWindow",
            "prevWindow",
            "sizerMenu",
            "undo",
            "fileDialog",
            "toggleScreen",
            "suppress",
            "nextDesktop",
            "prevDesktop",
            "quit",
            "switchToDesktop1..9",
            "moveToDesktop1..9",
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Pixels of outer margin between a snapped window and the work area.
    pub gap: i32,
    /// Modifiers held with the left mouse button to drag-move a window.
    pub drag_modifier: Mods,
    /// Put file dialogs into Explorer's current folder (Windows).
    pub quick_switch: bool,
    /// Milliseconds between always-on-bottom re-asserts (Windows).
    pub bottom_reassert_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            gap: 0,
            // A bare Win tap opens the Start menu; Win+Alt is a safe default
            // and still one-handed.
            drag_modifier: Mods {
                ctrl: false,
                shift: false,
                alt: true,
                win: true,
            },
            quick_switch: true,
            bottom_reassert_ms: 250,
        }
    }
}

pub struct Config {
    pub bindings: Vec<Binding>,
    pub settings: Settings,
}

pub struct Binding {
    pub combo: Combo,
    pub action: Action,
}

/// `path` if it exists, else the default TOML written to `path` first.
pub fn load_or_create(path: &Path, default_toml: &str) -> Result<String> {
    if !path.exists() {
        std::fs::write(path, default_toml)
            .with_context(|| format!("cannot create default {}", path.display()))?;
    }
    std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

/// Sizer presets live next to `desktop-tools.toml`, in their own file.
pub fn load_or_create_presets(path: &Path, default_toml: &str) -> Result<Vec<SizeSpec>> {
    let text = load_or_create(path, default_toml)?;
    parse_presets(&text)
}

/// Re-read `resize.toml`; the daemon calls this on each Sizer hotkey.
pub fn load_presets(path: &Path) -> Result<Vec<SizeSpec>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_presets(&text)
}

/// Presets are kept in file order: the menu order is the user's order.
/// Bad entries are skipped, like bad bindings in `desktop-tools.toml`.
pub fn parse_presets(text: &str) -> Result<Vec<SizeSpec>> {
    let value: toml::Value = text.parse().context("resize.toml is not valid TOML")?;
    let table = value
        .as_table()
        .context("resize.toml root must be a TOML table")?;
    let items = table
        .get("presets")
        .and_then(|v| v.as_array())
        .context("resize.toml needs a `presets = [...]` array")?;
    let mut out = Vec::new();
    for item in items {
        match item.as_str() {
            Some(s) => match s.parse::<SizeSpec>() {
                Ok(spec) => out.push(spec),
                Err(e) => eprintln!("warn: skipping preset `{s}`: {e:#}"),
            },
            None => eprintln!("warn: skipping preset: expected a string"),
        }
    }
    Ok(out)
}

/// `<exe_dir>/desktop-tools.toml` — the config lives next to the exe.
pub const CONFIG_NAME: &str = "desktop-tools.toml";
/// Older names; migrated once by `default_config_path`, newest first.
const OLD_CONFIG_NAMES: &[&str] = &["window-tools.toml", "windowtools.toml", "rectangle.toml"];

pub fn default_config_path() -> Result<PathBuf> {
    let path = exe_dir_file(CONFIG_NAME)?;
    if !path.exists() {
        for name in OLD_CONFIG_NAMES {
            let old = exe_dir_file(name)?;
            if old.exists() {
                if let Err(e) = std::fs::rename(&old, &path) {
                    eprintln!(
                        "warn: cannot rename {} to {}: {e}",
                        old.display(),
                        path.display()
                    );
                }
                break;
            }
        }
    }
    Ok(path)
}

pub fn exe_dir_file(name: &str) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate own executable")?;
    let dir = exe.parent().context("executable has no parent directory")?;
    Ok(dir.join(name))
}

/// Re-read at runtime. Unlike startup this never creates the file, so a
/// deleted config keeps the old bindings.
pub fn reload(path: &Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse(&text)
}

/// Modification time for the 1 s hot-reload poll. mtime only: FAT/exFAT has
/// 2 s granularity, so a second save inside that window can be missed.
pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// One message listing every hotkey that could not be registered.
pub fn conflict_message(failed: &[String]) -> Option<String> {
    if failed.is_empty() {
        return None;
    }
    Some(format!(
        "These hotkeys could not be registered (in use by another process, or unsupported):\n\n  - {}\n\nClose the other process or edit desktop-tools.toml.",
        failed.join("\n  - ")
    ))
}

/// Parse the flat TOML table. Bad entries are reported to stderr and skipped:
/// one malformed binding must not take down the daemon.
pub fn parse(text: &str) -> Result<Config> {
    let value: toml::Value = text.parse().context("config is not valid TOML")?;
    let table = value
        .as_table()
        .context("config root must be a TOML table of `combo = \"action\"`")?;
    let mut out = Vec::new();
    let mut settings = Settings::default();
    for (combo, action) in table {
        if combo == "gap" {
            match action.as_integer() {
                Some(n) => settings.gap = n.clamp(0, 4096) as i32,
                None => eprintln!("warn: `gap` must be an integer; using 0"),
            }
            continue;
        }
        if combo == "quickSwitch" {
            match action.as_bool() {
                Some(on) => settings.quick_switch = on,
                None => eprintln!("warn: `quickSwitch` must be true or false; using true"),
            }
            continue;
        }
        if combo == "bottomReassertMs" {
            match action.as_integer() {
                // Below 100 ms the Z-order spam outweighs the tighter pinning.
                Some(n) => settings.bottom_reassert_ms = n.clamp(100, 60_000) as u64,
                None => eprintln!("warn: `bottomReassertMs` must be an integer; using 1000"),
            }
            continue;
        }
        if combo == "dragModifier" {
            match action.as_str() {
                Some(s) => match s.parse::<Mods>() {
                    Ok(m) => settings.drag_modifier = m,
                    Err(e) => eprintln!("warn: `dragModifier`: {e}; using win+alt"),
                },
                None => eprintln!("warn: `dragModifier` must be a string; using win+alt"),
            }
            continue;
        }
        if let Some(t) = action.as_str() {
            match (combo.parse::<Combo>(), t.parse::<Action>()) {
                (Ok(c), Ok(a)) => out.push(Binding {
                    combo: c,
                    action: a,
                }),
                (Err(e), _) => eprintln!("warn: skipping binding `{combo}`: {e}"),
                (_, Err(e)) => eprintln!("warn: skipping binding `{combo}`: {e}"),
            }
        } else {
            eprintln!("warn: skipping binding `{combo}`: action must be a string");
        }
    }
    // TOML tables iterate in key order, not file order; sort so hotkey ids
    // are stable across runs and edits.
    out.sort_by_key(|b| b.combo);
    Ok(Config {
        bindings: out,
        settings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Key;

    #[test]
    fn parses_default_config() {
        let cfg = parse(include_str!("../desktop-tools.toml")).unwrap();
        assert_eq!(cfg.bindings.len(), 108);
        assert_eq!(cfg.settings.gap, 0);
        assert!(cfg.settings.quick_switch);
        assert!(cfg.settings.drag_modifier.win && !cfg.settings.drag_modifier.alt);
        let b = cfg
            .bindings
            .iter()
            .find(|b| b.action == Action::MoveToLeft)
            .expect("moveToLeft binding");
        assert_eq!(b.combo.key, Key::Char('h'));
        assert!(b.combo.win && b.combo.alt);
    }

    #[test]
    fn parses_gap_and_clamps_it() {
        let cfg = parse("gap = 10\n\"ctrl+alt+left\" = \"moveToLeft\"\n").unwrap();
        assert_eq!(cfg.settings.gap, 10);
        assert_eq!(cfg.bindings.len(), 1);

        assert_eq!(parse("gap = -5\n").unwrap().settings.gap, 0);
        assert_eq!(parse("gap = 999999\n").unwrap().settings.gap, 4096);
        // Wrong type: warn, keep the default, still accept bindings.
        assert_eq!(
            parse("gap = \"wide\"\n\"ctrl+a\" = \"undo\"\n")
                .unwrap()
                .settings
                .gap,
            0
        );
    }

    #[test]
    fn parses_numbered_desktop_actions() {
        for n in 1..=9u8 {
            let cfg = parse(&format!("\"ctrl+alt+{n}\" = \"switchToDesktop{n}\"\n")).unwrap();
            assert_eq!(cfg.bindings[0].action, Action::SwitchToDesktop(n));
            let cfg = parse(&format!("\"ctrl+shift+{n}\" = \"moveToDesktop{n}\"\n")).unwrap();
            assert_eq!(cfg.bindings[0].action, Action::MoveToDesktop(n));
        }
        assert!("switchToDesktop0".parse::<Action>().is_err());
        assert!("switchToDesktop10".parse::<Action>().is_err());
        assert!("moveToDesktopX".parse::<Action>().is_err());
    }

    #[test]
    fn parses_quick_switch() {
        assert!(parse("").unwrap().settings.quick_switch);
        assert!(
            !parse("quickSwitch = false\n")
                .unwrap()
                .settings
                .quick_switch
        );
        // A wrong type keeps the default instead of failing the config.
        assert!(
            parse("quickSwitch = \"no\"\n")
                .unwrap()
                .settings
                .quick_switch
        );
    }

    #[test]
    fn parses_bottom_reassert_ms_and_clamps_it() {
        assert_eq!(parse("").unwrap().settings.bottom_reassert_ms, 250);
        assert_eq!(
            parse("bottomReassertMs = 250\n")
                .unwrap()
                .settings
                .bottom_reassert_ms,
            250
        );
        assert_eq!(
            parse("bottomReassertMs = 1\n")
                .unwrap()
                .settings
                .bottom_reassert_ms,
            100
        );
        assert_eq!(
            parse("bottomReassertMs = 999999\n")
                .unwrap()
                .settings
                .bottom_reassert_ms,
            60_000
        );
    }

    #[test]
    fn parses_drag_modifier() {
        let cfg = parse("dragModifier = \"win\"\n").unwrap();
        assert!(cfg.bindings.is_empty());
        assert!(cfg.settings.drag_modifier.win && !cfg.settings.drag_modifier.alt);

        // Missing setting: Win+Alt.
        let d = parse("").unwrap().settings.drag_modifier;
        assert!(d.win && d.alt);
        // Invalid value keeps the default instead of dropping the config.
        let d = parse("dragModifier = \"hyper\"\n")
            .unwrap()
            .settings
            .drag_modifier;
        assert!(d.win && d.alt);
    }

    #[test]
    fn default_resize_file_parses() {
        let p = parse_presets(include_str!("../resize.toml")).unwrap();
        assert_eq!(p.len(), 10);
        assert_eq!(p[0].to_string(), "1600x1200");
        assert_eq!(p[5].to_string(), "3/4");
    }

    #[test]
    fn parses_resize_presets_in_order() {
        let p = parse_presets("presets = [\"1600x1200\", \"1/2\", \"3/4\", \"bogus\"]\n").unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].to_string(), "1600x1200");
        assert_eq!(p[1].to_string(), "1/2");
        assert_eq!(p[2].to_string(), "3/4");

        assert!(parse_presets("nope = []\n").is_err());
        assert!(parse_presets("presets = \"1/2\"\n").is_err());
    }

    #[test]
    fn skips_bad_entries_and_continues() {
        let text = "\"ctrl+alt+left\" = \"moveToLeft\"\n\"bogus\" = \"moveToLeft\"\n\"ctrl+a\" = \"noSuchAction\"\n";
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.bindings.len(), 1);
        assert_eq!(cfg.bindings[0].action, Action::MoveToLeft);
    }

    #[test]
    fn conflict_message_lists_every_failure_once() {
        assert_eq!(conflict_message(&[]), None);
        let msg = conflict_message(&[
            "ctrl+alt+left".to_string(),
            "win+f2 (unknown key)".to_string(),
        ])
        .expect("message");
        assert_eq!(msg.matches("ctrl+alt+left").count(), 1);
        assert!(msg.contains("win+f2 (unknown key)"));
    }

    #[test]
    fn load_or_create_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("desktop-tools-rs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("desktop-tools.toml");
        let _ = std::fs::remove_file(&path);

        let text = load_or_create(&path, "x = 1\n").unwrap();
        assert_eq!(text, "x = 1\n");
        assert!(path.exists());
        std::fs::write(&path, "y = 2\n").unwrap();
        assert_eq!(load_or_create(&path, "x = 1\n").unwrap(), "y = 2\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Key and key-combo primitives. Adapted from keytools-rs `kit::keys`
//! (keymap-rs convention): `ctrl+alt+left`, `win+f2`, `^!+g` (AHK sigils).

use anyhow::{bail, Result};
use std::fmt;
use std::str::FromStr;

/// A key identity. Letters carry a lowercase ASCII char; function keys F1-F24.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Key {
    Char(char),
    F(u8),
    Insert,
    Delete,
    Space,
    Tab,
    Backspace,
    Escape,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Snapshot,
    Pause,
    Other,
}

/// A modifier+key combination like `ctrl+alt+left`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Combo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
    pub key: Key,
}

impl Default for Combo {
    fn default() -> Self {
        Combo {
            ctrl: false,
            shift: false,
            alt: false,
            win: false,
            key: Key::Other,
        }
    }
}

impl FromStr for Combo {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Combo> {
        // AHK sigil form first: modifiers are the leading run of ^ ! + #.
        let trimmed = s.trim();
        if trimmed
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '^' | '!' | '+' | '#'))
        {
            let mut c = Combo::default();
            let mut rest = trimmed;
            while let Some(ch) = rest.chars().next() {
                match ch {
                    '^' => c.ctrl = true,
                    '!' => c.alt = true,
                    '+' => c.shift = true,
                    '#' => c.win = true,
                    _ => break,
                }
                rest = &rest[ch.len_utf8()..];
            }
            let name = rest.trim().to_ascii_lowercase();
            c.key = parse_key(&name)
                .ok_or_else(|| anyhow::anyhow!("unknown key `{name}` in combo `{s}`"))?;
            return Ok(c);
        }

        let mut c = Combo::default();
        let mut key_name: Option<String> = None;
        for part in s.split('+') {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "" => continue,
                "ctrl" | "control" | "^" => c.ctrl = true,
                "shift" => c.shift = true,
                "alt" | "!" => c.alt = true,
                "win" | "meta" | "super" | "#" => c.win = true,
                name => {
                    if key_name.is_some() {
                        bail!("combo `{s}` has more than one non-modifier key");
                    }
                    key_name = Some(name.to_string());
                }
            }
        }
        let name = key_name.ok_or_else(|| anyhow::anyhow!("combo `{s}` is missing a key"))?;
        c.key = parse_key(&name)
            .ok_or_else(|| anyhow::anyhow!("unknown key `{name}` in combo `{s}`"))?;
        Ok(c)
    }
}

fn parse_key(name: &str) -> Option<Key> {
    let b = name.as_bytes();
    if b.len() == 1 && b[0].is_ascii_graphic() && !matches!(b[0], b'+' | b'^' | b'!' | b'#') {
        return Some(Key::Char(b[0].to_ascii_lowercase() as char));
    }
    if let Some(rest) = name.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Some(Key::F(n));
            }
        }
        return None;
    }
    Some(match name {
        "insert" | "ins" => Key::Insert,
        "delete" | "del" => Key::Delete,
        "space" => Key::Space,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" | "esc" => Key::Escape,
        "enter" | "return" => Key::Enter,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "prtscn" | "printscreen" | "snapshot" => Key::Snapshot,
        "pause" | "break" => Key::Pause,
        _ => return None,
    })
}

/// X11 keysym for this key (for XGrabKey / keyboard mapping lookup).
#[cfg(target_os = "linux")]
pub fn keysym_of(key: Key) -> Option<u32> {
    Some(match key {
        Key::Char(c) => c as u32,
        Key::F(n) => 0xffbe + n as u32 - 1,
        Key::Insert => 0xff63,
        Key::Delete => 0xffff,
        Key::Space => 0x20,
        Key::Tab => 0xff09,
        Key::Backspace => 0xff08,
        Key::Escape => 0xff1b,
        Key::Enter => 0xff0d,
        Key::Left => 0xff51,
        Key::Right => 0xff53,
        Key::Up => 0xff52,
        Key::Down => 0xff54,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::Snapshot => 0xff61,
        Key::Pause => 0xff13,
        Key::Other => return None,
    })
}

/// A modifier-only chord (`win`, `win+alt`) for mouse gestures.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}

impl Mods {
    pub fn is_empty(&self) -> bool {
        !(self.ctrl || self.shift || self.alt || self.win)
    }
}

impl FromStr for Mods {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Mods> {
        let mut m = Mods::default();
        for part in s.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "" => continue,
                "ctrl" | "control" | "^" => m.ctrl = true,
                "shift" => m.shift = true,
                "alt" | "!" => m.alt = true,
                "win" | "meta" | "super" | "#" => m.win = true,
                other => bail!("`{other}` is not a modifier in `{s}`"),
            }
        }
        anyhow::ensure!(
            !m.is_empty(),
            "`{s}` needs at least one of ctrl/shift/alt/win"
        );
        Ok(m)
    }
}

impl fmt::Display for Combo {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.shift {
            parts.push("shift");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.win {
            parts.push("win");
        }
        parts.push(match self.key {
            Key::Char(ch) => {
                return write!(
                    f,
                    "{}{}{}",
                    parts.join("+"),
                    if parts.is_empty() { "" } else { "+" },
                    ch
                )
            }
            Key::F(n) => {
                return write!(
                    f,
                    "{}{}f{n}",
                    parts.join("+"),
                    if parts.is_empty() { "" } else { "+" }
                )
            }
            Key::Insert => "insert",
            Key::Delete => "delete",
            Key::Space => "space",
            Key::Tab => "tab",
            Key::Backspace => "backspace",
            Key::Escape => "escape",
            Key::Enter => "enter",
            Key::Left => "left",
            Key::Right => "right",
            Key::Up => "up",
            Key::Down => "down",
            Key::Home => "home",
            Key::End => "end",
            Key::PageUp => "pageup",
            Key::PageDown => "pagedown",
            Key::Snapshot => "prtscn",
            Key::Pause => "pause",
            Key::Other => "?",
        });
        write!(f, "{}", parts.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let c: Combo = "ctrl+alt+left".parse().unwrap();
        assert!(c.ctrl && c.alt && !c.win && !c.shift);
        assert_eq!(c.key, Key::Left);
        let c: Combo = "win+f2".parse().unwrap();
        assert!(c.win && c.key == Key::F(2));
        let c: Combo = "ctrl+alt+shift+q".parse().unwrap();
        assert!(c.ctrl && c.alt && c.shift && c.key == Key::Char('q'));
        let c: Combo = "win+prtscn".parse().unwrap();
        assert!(c.win && c.key == Key::Snapshot);
        let c: Combo = "win+pause".parse().unwrap();
        assert!(c.key == Key::Pause);
        let c: Combo = "win+.".parse().unwrap();
        assert!(c.key == Key::Char('.'));
        let c: Combo = "win+shift+=".parse().unwrap();
        assert!(c.win && c.shift && c.key == Key::Char('='));
    }

    #[test]
    fn parse_ahk_sigils() {
        let c: Combo = "^!+g".parse().unwrap();
        assert!(c.ctrl && c.alt && c.shift && c.key == Key::Char('g'));
        let c: Combo = "#f2".parse().unwrap();
        assert!(c.win && c.key == Key::F(2));
    }

    #[test]
    fn parse_punctuation_key() {
        let c: Combo = "alt+`".parse().unwrap();
        assert!(c.alt && !c.shift && c.key == Key::Char('`'));
        let c: Combo = "alt+shift+`".parse().unwrap();
        assert!(c.alt && c.shift && c.key == Key::Char('`'));
    }

    #[test]
    fn parse_aliases_and_names() {
        let c: Combo = "ctrl+alt+enter".parse().unwrap();
        assert_eq!(c.key, Key::Enter);
        let c: Combo = "meta+return".parse().unwrap();
        assert!(c.win && c.key == Key::Enter);
        assert!("ctrl+a+b".parse::<Combo>().is_err());
        assert!("ctrl+".parse::<Combo>().is_err());
        assert!("f25".parse::<Combo>().is_err());
    }

    #[test]
    fn parse_modifier_only_chords() {
        let m: Mods = "win+alt".parse().unwrap();
        assert!(m.win && m.alt && !m.ctrl && !m.shift);
        let m: Mods = "ctrl+shift".parse().unwrap();
        assert!(m.ctrl && m.shift && !m.alt && !m.win);
        assert!("win+a".parse::<Mods>().is_err());
        assert!("none".parse::<Mods>().is_err());
        assert!("".parse::<Mods>().is_err());
    }

    #[test]
    fn display_roundtrip() {
        for s in [
            "ctrl+alt+left",
            "win+f2",
            "ctrl+alt+shift+q",
            "alt+enter",
            "alt+`",
        ] {
            let c: Combo = s.parse().unwrap();
            let c2: Combo = c.to_string().parse().unwrap();
            assert_eq!(c, c2, "`{s}` displayed as `{c}`");
        }
    }
}

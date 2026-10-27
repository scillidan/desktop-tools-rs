# desktop-tools-rs

A Rust implementation of a collection of tools related to window and desktop.

> [!WARNING]
> Not yet tested on Linux.

Authors: DeepSeek-4.1-Flash🧙‍♂️, Kimi-K2.7-Code🧙‍♂️, scillidan🤡

## Keys (defaults)

| Keys | Action | Behavior |
| :- | :- | :- |
| `win+alt+h/l` | `moveToLeft` / `moveToRight` | half; repeat cycles 1/2 → 2/3 → 1/3 |
| `win+alt+k/j` | `moveToTop` / `moveToBottom` | half; same cycle |
| `ctrl+win+alt+h/k/j/l` | `moveToTopLeft` … `moveToBottomRight` | quarter; repeat cycles width 1/2 → 2/3 → 1/3 |
| `ctrl+win+alt+c` | `moveToCenter` | center, keep size |
| `ctrl+win+alt+f` | `maximize` | toggle maximize |
| `ctrl+win+alt+m` | `minimize` | minimize to taskbar |
| `ctrl+win+alt+t` | `toggleAlwaysOnTop` | toggle topmost |
| `ctrl+win+alt+n/p` | `nextDisplay` / `prevDisplay` | next/previous monitor, scaled by work area |
| `ctrl+win+alt+s` | `sizerMenu` | size presets at the cursor (Windows); cycles on X11 |
| `ctrl+win+alt+u` | `undo` | step back through snap/maximize/minimize history |
| `ctrl+alt+l` | `fileDialog` | apply the remembered folder to the dialog in front (Windows); passed through to the app when no dialog is in front |
| `win+f2` | `toggleScreen` | power all monitors off; press again to wake (Windows). Windows stranded by a standby-unplugged monitor are put back on wake |
| `win+alt+1..9` | `switchToDesktop1..9` | switch virtual desktop |
| `win+`` / `win+shift+`` | `nextDesktop` / `prevDesktop` | cycle virtual desktops, wrapping around |
| `alt+shift+1..9` | `moveToDesktop1..9` | move the active window there |
| `alt+`` / `alt+shift+`` | `nextWindow` / `prevWindow` | cycle visible windows of the same app |
| any Win combo | `suppress` | swallow the combo without acting — disable shell shortcuts you never use (e.g. `"win+=" = "suppress"` keeps Magnifier closed) |
| unbound | `unpinAll` | clear topmost on every window |
| unbound | `quit` | exit the daemon |

Snap cycles advance only when the same action repeats on the same window.
Changing direction, switching between edge/corner snapping, using another
resize action, or changing windows restarts at one half.

Virtual desktops are numbered left to right; `switchToDesktopN` and
`moveToDesktopN` create desktops up to N when missing. Windows 10 and 11 use
different internal interface IDs; X11 uses `_NET_CURRENT_DESKTOP` /
`_NET_WM_DESKTOP`.

## Win combos vs Explorer (Windows)

Windows reserves the Win+key namespace (taskbar digits, jump lists, ...), and
the shell reacts even when an app successfully `RegisterHotKey`s one of those
combos. A low-level keyboard hook therefore suppresses every Win-containing
binding from the config before Explorer can see it; the action still runs, and
Explorer's own reaction (e.g. a jump list) is what gets swallowed. Injected
input is ignored, so the daemon's own synthesized keys are not eaten.

## Mouse drag (Windows)

Hold **Win** and drag with the left button to move the window under the
cursor, across monitors. A maximized window restores on the first movement with
the grab point at the same proportional spot; a click without dragging only
focuses. The press is swallowed, so the window gets no click. `dragModifier`
changes the chord; the swallowed click injects a mask key (vk07, AutoHotkey's
MenuMaskKey) so a bare Win release does not open the Start menu. Tray Suspend
disables the gesture. Moving elevated windows needs the daemon to run elevated
too.

## File dialogs (Windows)

When a Save/Open dialog opens, it is sent to the folder of the most recently
active file manager (QuickSwitch auto mode): Explorer via `ShellWindows`,
Double Commander via its window title, or a terminal whose title is a directory
(yazi's default). The suggested file name is left alone. If auto mode misses,
`ctrl+alt+l` (`fileDialog`) applies the remembered folder to the dialog in
front. `quickSwitch = false` turns auto mode off.

## Config (`desktop-tools.toml`)

Flat TOML: `"combo" = "action"` entries plus optional settings. Combo syntax:
`ctrl+alt+h`, `win+f2`, or AHK sigils `^!+g`. A bad entry is skipped with a
warning, never fatal.

| Key | Default | Behavior |
| :- | :- | :- |
| `gap` | `0` | inset the work area by N px on every side |
| `dragModifier` | `"win"` | modifier held with the left button to drag-move (Windows) |
| `quickSwitch` | `true` | put Save/Open dialogs into the last file-manager folder (Windows) |

The file is re-read within ~1 s of saving; a TOML error keeps the old config
running and is reported in the tray/stderr.

### Sizer presets (`resize.toml`)

`sizerMenu` reads `presets = [...]` next to `desktop-tools.toml`, in menu order:

```toml
presets = [
    "1600x1200",
    "1600x900",
    "1440x900",
    "1280x720",
    "1024x768",
    "3/4",
    "2/3",
    "1/2",
    "1/3",
    "1/4",
]
```

Exact sizes keep the current top-left corner. The file is re-read on every
press, so edits apply without a reload. Bad entries are skipped.

## Platform notes

- **Windows**: 10 1607+ (Per-Monitor V2). DWM invisible borders are compensated,
  so edges align exactly; monitors use their work area, taskbars excluded.
- **Linux**: X11 only (Wayland does not let clients move other apps' windows by
  design). Multi-monitor via RANDR, work area via `_NET_WORKAREA`.

## Development

```sh
cargo test # 46 unit tests, 4 --ignored real-machine/regeneration tests
cargo build # debug exe keeps a console
cargo check --target x86_64-unknown-linux-gnu
```

`src/layout.rs` geometry, `src/platform.rs` the `Platform` trait plus the
snapping/undo state machine, `src/win32.rs` and `src/x11.rs` the backends,
`src/mouse.rs`, `src/dialogs.rs`, `src/vdesk.rs`, `src/screen.rs` and
`src/tray.rs` the Windows features, `src/config.rs` and `src/keys.rs` TOML
and combo parsing, `build.rs` + `assets/desktop-tools.ico` the embedded exe
icon (regenerate with `cargo test write_icon_asset -- --ignored`).

Before a release, walk through [CHECKLIST.md](CHECKLIST.md).
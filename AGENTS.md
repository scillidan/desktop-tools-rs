# AGENTS.md — desktop-tools-rs

Rust successor of RectangleWin/RectangleWinPlus: global-hotkey desktop tools
(window snapping, virtual desktops, monitor power, file dialogs) for Windows
and Linux X11. Cross-platform from day one; the two backends implement one
`Platform` trait.

## Commands

- `cargo build` / `cargo build --release` — release is GUI-subsystem on Windows
- `cargo test` — unit tests only; CI runs them plus fmt/clippy on Windows and Linux
- Release: push a `v*` tag; the release workflow builds the Windows zip
  (exe + `desktop-tools.toml` + `resize.toml`) and uploads it with notes
- `cargo check --target x86_64-unknown-linux-gnu` — verifies the X11 backend
  on a Windows host (needs `rustup target add x86_64-unknown-linux-gnu`)

## Conventions (inherited from keytools-rs / keymap-rs)

- `anyhow` errors with context; a failed hotkey action logs a warning and the
  daemon keeps running — never crash the loop.
- Config: TOML, created next to the exe on first run: `desktop-tools.toml`
  (bindings + `gap` + `dragModifier`, seeded from
  `include_str!("../desktop-tools.toml")`) and `resize.toml` (Sizer presets, seeded
  from `include_str!("../resize.toml")`). One bad binding or preset is skipped
  with a stderr warning, never fatal. Older config names (`window-tools.toml`,
  `windowtools.toml`, `rectangle.toml`) are renamed once on first start.
- Tests are inline `#[cfg(test)] mod tests` next to the code.
- Release profile: `lto = true`, `codegen-units = 1`, `strip = true`.
- The exe icon is `assets/desktop-tools.ico`, embedded by `build.rs` via the
  `winresource` crate; regenerate it with
  `cargo test write_icon_asset -- --ignored` after editing the glyph in
  `src/tray.rs`.

## Layout

- `src/layout.rs` — pure rect geometry (`to_left/to_right/...`, `merge`,
  `center`, `intersect`) and Sizer `SizeSpec` parsing (`1600x1200`, or `1/2`
  = half the work-area width and height). No platform types. Windows rect
  convention: inclusive-exclusive (r = l + w).
- `src/platform.rs` — `Platform` trait (window/monitors/hotkey primitives)
  and `App<P>`, the shared snapping state machine (cycle counters, target
  selection, same-app window cycling, Sizer presets). Tested against a mock
  `Platform`.
- `src/win32.rs` — Windows backend (`windows` crate 0.59): Per-Monitor-V2
  DPI at startup, `RegisterHotKey` + GetMessageW loop, single-instance
  mutex, DWM `EXTENDED_FRAME_BOUNDS` border compensation. Owns the daemon
  loop: tray/menu events, `SetTimer` 1 s config reload + screen-power tick,
  suspend/resume.
- `src/mouse.rs` — Windows `WH_MOUSE_LL` drag-move (default Win+LMB,
  configurable via `dragModifier`): swallows the press/release, restores a
  maximized window on the first move (the grab point keeps its proportional
  place), allowed across monitors. Installed and removed with the daemon and
  tray suspend. A swallowed click while Win is held injects the vk07 mask key
  so releasing Win does not open the Start menu.
- `src/kbdhook.rs` — Windows `WH_KEYBOARD_LL` suppression hook for
  Win-containing bindings: Explorer owns the Win+key namespace (taskbar
  digits, jump lists) and reacts even when `RegisterHotKey` succeeds, so the
  hook swallows the trigger key of any bound Win combo before the shell sees
  it and posts the binding index to the daemon loop as `WM_KBDHOOK`. Injected
  events are ignored (the daemon's own SendInput must not be eaten). The
  matcher is a pure, unit-tested state machine. Any Win combo may be bound to
  `suppress` to swallow it with no action (killing shell shortcuts); Win+L
  and Win+Ctrl+Shift+B are owned by winlogon/dwm and cannot be hooked.
- `src/vdesk.rs` — Windows 10/11 native virtual desktops: the undocumented
  `IVirtualDesktopManagerInternal` switches, creates and moves windows (via
  `IApplicationViewCollection::GetViewForHwnd` + `MoveViewToDesktop`); missing
  desktops are created on demand. Windows 11 uses another manager/desktop IID
  pair with the same vtable. Raw COM vtables, no new dependency; two `--ignored`
  tests switch desktops and move a temporary real window.
- `src/screen.rs` — `toggleScreen` (default `win+f2`): posts
  `SC_MONITORPOWER` to Progman to power all monitors off, re-asserts the
  power-off every 1 s while toggled (any input wakes the screens), and
  restores windows stranded on the primary monitor when a standby monitor
  unplugs itself (DisplayPort link drop). Windows-only.
- `src/dialogs.rs` — Windows file-dialog helper (QuickSwitch auto mode):
  250 ms foreground watch, folder from Explorer (`ShellWindows`), Double
  Commander (window title) or a terminal title that is a directory; the folder
  is typed into the dialog's File name box and confirmed with Enter.
  `alt+shift+l` (`fileDialog`) retries manually; with no dialog in front the
  combo is re-injected into the foreground app (pass-through, so DoubleCmd's
  own `alt+shift+l` accelerator still works). Auto-fill runs on a worker and
  cross-process control messages time out, so the daemon's low-level input
  hooks stay responsive. `quickSwitch = false` disables auto mode.
- `src/tray.rs` — Windows tray icon + six-item menu (start with Windows,
  reload, edit, unpin all, suspend/resume, quit), the abstract window glyph
  (hollow title bar + filled body; also the exe icon, see `ico_bytes`), and
  the HKCU Run autostart helpers.
- `src/x11.rs` — X11 backend (`x11rb` 0.13, feature `randr`): passive grabs
  with LockMask/Mod2Mask variants, RANDR monitors, `_NET_WORKAREA`,
  `_NET_WM_STATE` client messages, non-blocking event poll + ungrab for
  reload. Wayland is out of scope by design.
- `src/keys.rs` — `Combo` parser (`ctrl+alt+left`, `^!+g`), shared by both
  backends (Windows VK / X11 keysym mappings live next to it).
- `src/config.rs` — `Action` enum (move/maximize names match RectangleWinPlus;
  `minimize`, `unpinAll`, `nextWindow`, `prevWindow`, `sizerMenu`, `undo`,
  numbered `switchToDesktopN`/`moveToDesktopN`, `toggleScreen`,
  `toggleAlwaysOnBottom`, `nextDesktop`/`prevDesktop`, `quit` are additions), `gap` setting,
  `dragModifier`, `quickSwitch`, `resize.toml` preset parsing, TOML parse,
  reload/mtime/conflict-report helpers.

## Compatibility contract (do not regress)

These were empirically verified on the maintainer's machine; keep the tests
green and do not reintroduce stricter checks without a comment:

1. **WezTerm must be movable** — its style is `WS_POPUP` without caption
   chrome; the OS built-in snap skips it. We move via SetWindowPos, so fine.
2. **QuteBrowser (Qt) must be movable** — style `WS_POPUP|WS_THICKFRAME`
   with no min/max boxes. RectangleWin's FancyZones-derived
   `isStandardWindow` rejected exactly this; our `zonable` must NOT filter
   on popup/thickframe/min-max bits. Only reject: not-root-ancestor,
   invisible, minimized, desktop/shell classes, `WS_EX_TOOLWINDOW`.
3. Multi-monitor: monitor_index wraps with `rem_euclid`, so nextDisplay on
   the last monitor goes to the first.
4. **Do not poll a swallowed mouse button** — once the `WH_MOUSE_LL` hook
   returns nonzero for `WM_LBUTTONDOWN`, `GetAsyncKeyState(VK_LBUTTON)` may
   keep reporting "up". The drag ends on the hook's own `WM_LBUTTONUP`, never
   on a poll (polling cancelled every drag on the first move, verified
   2026-10-25). Drag-move works on WezTerm and QuteBrowser with `win+alt`.
5. **Each WezTerm window is its own process** (`wezterm-gui.exe`), so the
   PID match in `same_app_windows` finds only one window; the executable
   image-name fallback is what makes Alt+` cycle them (verified 2026-10-30).

## What was deliberately NOT ported from RectangleWinPlus

Settings GUI, YAML config, URL/gist config import, autoupdate, installer
packaging, `pushTo*`, `makeSmaller/Larger`, `makeFullHeight`,
`almostMaximize`, `preview`/`preview_ms` (P1 cut). Add only on request.

Windows has a tray icon with the six items above; Linux has no tray yet (TODO,
would need GTK/AppIndicator deps). Windows and Linux both hot-reload the TOML.

## Windows shell note

This repo's parent AGENTS.md applies: never use bare `find` on Windows.

# Manual checklist

Run after `cargo build --release`, with the release exe + `desktop-tools.toml`
next to each other. Run every item, not just the ones in the area you
touched, before calling a release good.

## Tray (Windows)

- [ ] Right-click the tray icon: Start with Windows, Reload config, Edit
      config, Unpin all, Suspend/Resume hotkeys, Quit are all present.
- [ ] Every row is enabled (dark text, clickable), not greyed out.
- [ ] Suspend: the whole icon turns grey. Resume: back to blue.
- [ ] Save a valid change: a yellow dot appears at the bottom right for at
      least 1 s.
- [ ] Save a broken TOML: the dot turns red for at least 1 s.
- [ ] **Start with Windows** toggles the check mark and writes/removes
      `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
      `desktop-tools-rs` (verify with `reg query`).
- [ ] **Edit config** opens `desktop-tools.toml` in `$EDITOR`/`$VISUAL` when set,
      else in the default handler.
- [ ] **Suspend hotkeys** releases the combos: the arrows stop snapping and
      another app can grab e.g. `ctrl+alt+left`; the drag gesture no longer
      moves windows. **Resume** restores both.
- [ ] **Quit** exits the process and removes the icon from the tray.

## Config reload

- [ ] With the daemon running, change a hotkey and save: the new combo works
      within ~1 s (old combo no longer fires).
- [ ] Add `gap = 8` and save: the next snap leaves an 8 px outer margin.
- [ ] Break the TOML (e.g. delete a quote) and save: the daemon keeps running
      with the old bindings, tray tooltip/stderr say reload failed.
- [ ] Replace the file with one that has no valid bindings: old config stays
      active; no crash.
- [ ] Delete the file: daemon keeps the old config; recreating it reloads.

## Hotkey conflicts

- [ ] Start an app that owns one of desktop-tools-rs's combos, then start
      desktop-tools-rs: **one** message box lists every unavailable combo.
- [ ] The remaining hotkeys still work after dismissing the box.
- [ ] If *all* combos conflict, the daemon exits with the same list.

## Snapping regressions (must stay green)

- [ ] WezTerm (`WS_POPUP`, no caption) snaps with every edge/corner action.
- [ ] QuteBrowser (Qt, `WS_POPUP|WS_THICKFRAME`, no min/max boxes) snaps.
- [ ] DWM compensation: snapped edges align exactly with the work area edge
      (no 1-8 px invisible-border gap).
- [ ] Multi-monitor: `nextDisplay` on the last monitor wraps to the first
      (`rem_euclid`), no error.
- [ ] `nextDisplay` between monitors of different sizes keeps the same screen
      fraction (not the same pixels) and is centered on the target work area.
- [ ] Different per-monitor DPI: snap a window on each monitor, compare the
      top-left/bottom-right against the visible work area.
- [ ] Multi-monitor + `gap`: margin is identical on both monitors.
- [ ] `maximize` twice: the second press restores the pre-maximize rect.

## Minimize / unpin

- [ ] `ctrl+win+alt+m` minimizes the foreground window (incl. WezTerm and
      QuteBrowser, which rbtray cannot minimize); taskbar shows it restored.
- [ ] Pin two windows (DeskPins-style), then tray **Unpin all**: both lose
      topmost; other windows are untouched.

## Same-app window switching (optional; unbound by default)

- [ ] Add "alt+\`" = "nextWindow" and "alt+shift+\`" = "prevWindow" to
      `desktop-tools.toml` (they are commented out by default), then open two
      WezTerm windows: Alt+\` focuses the other one, repeated presses cycle
      both ways and wrap. Alt+Shift+\` goes back.
- [ ] Switch to another app: the next press starts from that app's windows,
      not the previous app's.
- [ ] An app with a single window: no error, the window comes to the front.

## Sizer presets

- [ ] `ctrl+win+alt+s` at the cursor shows the `resize.toml` presets in file
      order: the exact sizes first (1600x1200 …), then the fractions (both
      width and height).
- [ ] Choosing an exact size resizes the foreground window to those visible
      pixels; a fraction scales both the current monitor's work-area width
      and height. The top-left stays put.
- [ ] Edit `resize.toml` (e.g. put `"800x600"` first), press the hotkey again:
      the new menu order appears without restarting.
- [ ] Add a bad preset (`"bogus"`): it is skipped and the other entries still
      show.
- [ ] Dismiss the menu without choosing: the window does not change.
- [ ] `undo` after a preset restores the previous rect.
- [ ] On X11 (no native popup), repeated presses cycle the presets.

## Win+LMB drag (Windows)

- [ ] Hold Win and drag from the middle of a normal window: it follows the
      cursor, to another monitor too.
- [ ] Win+click without moving: the window gets focus, stays where it is,
      and does not receive the click.
- [ ] Win+LMB on a maximized window: it restores on the first movement at
      its normal size, with the grab point at the same proportional spot, and
      then follows the cursor.
- [ ] Release Win before the mouse button: the drag continues until the button
      is released, and the Start menu does not open.
- [ ] Set `dragModifier = "ctrl+alt"`: Win+LMB stops dragging, Ctrl+Alt+LMB
      starts.
- [ ] Win+LMB on the taskbar, desktop or a tray icon: nothing moves.
- [ ] WezTerm and QuteBrowser can be moved the same way.
- [ ] Tray **Suspend**: the drag gesture no longer moves anything; **Resume**
      restores the gesture.

## Virtual desktops (Windows 10)

- [ ] `win+alt+1` / `win+alt+2` switch to desktop 1 / 2 (all 1..9 are bound),
      and Explorer's own Win+Alt+number jump list does NOT open.
- [ ] `win+`` / `win+shift+`` cycle to the next / previous desktop, wrapping
      around the ends.
- [ ] `alt+`` with several windows of one app visible (e.g. two Explorer
      windows): each press cycles the focused window through them;
      `alt+shift+`` goes backwards; other apps' windows stay put.
- [ ] A bare Win tap still opens the Start menu; after a swallowed Win combo
      (e.g. `win+alt+1`) it does not.
- [ ] `win+e`, `win+left`, `win+g`, `win+1`, `win++` all do nothing (suppressed);
      `win+d`, `win+r`, `win+q`, `win+l`, `win+tab`, `win+space` still work.
- [ ] `alt+shift+2` on a window moves it to desktop 2; the current desktop
      does not change.
- [ ] `win+alt+3` on a single-desktop setup: desktops 2 and 3 are created and the
      switch lands on 3 (Task View shows three).
- [ ] Tray Suspend releases every combo: `win+alt+1` switches nothing and
      Explorer's own jump list opens again.
- [ ] On X11: the same actions switch/move through `_NET_CURRENT_DESKTOP` /
      `_NET_WM_DESKTOP` if the WM supports EWMH.
- [ ] Windows 11 (if available): the desktop actions report "unavailable"
      instead of crashing.

## Monitor power (Windows)

- [ ] `win+f2`: all monitors power off; any input briefly wakes them and they
      re-darken within ~1 s; `win+f2` again wakes them for good.
- [ ] Multi-monitor (DisplayPort): with windows on the secondary monitor,
      power off, then power on — the secondary monitor's windows return to
      their previous positions instead of piling up on the primary.

## File dialogs (Windows)

- [ ] Open Explorer at some folder, then open Notepad's Save As: the dialog
      opens in that folder and the suggested file name is unchanged.
- [ ] Focus Double Commander in a folder, open Sioyek's `o` dialog: it lands in
      Double Commander's active panel folder.
- [ ] Press `ctrl+alt+l` with a file dialog in front: it jumps to the
      remembered folder, even if the dialog was already open.
- [ ] Press `ctrl+alt+l` with Double Commander in front (no dialog): the
      combo reaches DoubleCmd (e.g. its "list view" accelerator) instead of
      doing nothing.
- [ ] Navigate Explorer/Double Commander to another folder, open a new dialog:
      the new folder is used.
- [ ] Open a dialog without using a file manager since the daemon started: it
      keeps its default folder, no error.
- [ ] Set `quickSwitch = false` and save (reload within 1 s): the next dialog
      keeps its own folder; `ctrl+alt+l` still applies the remembered one.
      Set it back: auto-switch returns.
- [ ] Tray **Suspend**: dialogs keep their own folder; **Resume** restores.
- [ ] yazi in WezTerm: if the terminal title is the current directory, the
      dialog follows it; otherwise the manual key still works after an
      Explorer/DC visit.
- [ ] An elevated app's dialog is left alone (warning in the tray/stderr, no
      crash).

## Undo

- [ ] Snap a window (e.g. left half), move it somewhere else manually if you
      like, press `undo`: the rect from **before** the snap is restored.
- [ ] Snap again and press `undo` repeatedly: each press steps one level back
      until the history is exhausted (capped at 32 entries).
- [ ] Snap the same window left twice (1/2 → 2/3), undo: back to the original
      rect, and the next left snap starts at 1/2 again.
- [ ] Maximize, then undo: window returns to its pre-maximize rect.
- [ ] Minimize window A, then window B; `undo` re-shows B first, a second
      `undo` re-shows A. A window minimized while snapped re-shows snapped;
      the next `undo` moves it back to its pre-snap rect.
- [ ] Close the snapped window, then undo: warning in the tray tooltip (and
      stderr when launched from a terminal), no crash.
- [ ] `desktop-tools-rs --action undo` (one-shot) refuses with a clear message.

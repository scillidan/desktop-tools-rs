# Roadmap — window tools

Design record: what exists, what it was modeled on, and what is deliberately
out of scope.

## Status

- **P0 (done):** all six tools — `minimize`/`unpinAll`, same-app window cycle,
  Sizer presets, Win+Alt drag-move, numbered virtual desktops,
  QuickSwitch-style dialogs — plus `toggleScreen` (monitor power).
  46 unit tests; 4 `--ignored` real-machine/regeneration tests.
- **P1 (done):** renamed to `window-tools-rs` (package, exe, config, tray,
  autostart) with migration from the `windowtools-*` and `rectangle-*` names.
- **P1.5 (done, 2026-10-26):** renamed to `desktop-tools-rs` — monitor power
  and virtual desktops are outside a literal "window" scope, and the app is a
  PowerToys-style desktop-tools umbrella. Same migration pattern (exe, config
  `desktop-tools.toml`, `HKCU\Run`, single-instance mutex, tray, scoop
  manifest, release zip); tray/exe icon redrawn as a retro CRT monitor.
- **P2 (waiting):** move the `Platform` primitives into the keytools-rs `kit`
  when that host exists. No keysharp: the glue is TOML + one-shot CLI, and
  keysharp would add a .NET runtime and lose Linux.
- **P3 (next):** release packaging — a tag builds the Windows exe and uploads
  a zip (exe + `window-tools.toml` + `resize.toml`) to GitHub Releases;
  optional Scoop manifest.

## Reference tools

Usage habits and interaction matter as much as the implementation; this is
what we took from each.

- [ahmetb/RectangleWin](https://github.com/ahmetb/RectangleWin) — window
  resizing. Habit: the action set and names, repeat-to-cycle 1/2 → 2/3 → 1/3,
  maximize toggles back. Its FancyZones-derived `isStandardWindow` is the
  check we refuse to copy: it rejects QuteBrowser.
- [benbuck/rbtray](https://github.com/benbuck/rbtray) — minimize. Habit: get
  a window out of the way from the keyboard. We kept the plain minimize, not
  minimize-to-tray.
- [efotinis/deskpins](https://efotinis.neocities.org/deskpins) —
  always-on-top. Habit: one toggle pins a window above all others;
  `unpinAll` clears them all at once.
- [sigoden/window-switcher](https://github.com/sigoden/window-switcher) —
  same-app window switching. Habit: cycle inside the current app instead of
  the global Alt+Tab; Z-order, newest first, wraps, and resets when the app
  changes.
- macOS `Cmd+` — the original same-app cycle gesture; on Windows it is
  `nextWindow`/`prevWindow` (unbound by default).
- [alvesvaren/resizer2](https://github.com/alvesvaren/resizer2) — modifier +
  left-drag anywhere in a window. We took move (not resize) and made the
  modifier configurable (`dragModifier`, default `win`).
- [brianapps.net/sizer](https://brianapps.net/sizer/) — size presets. Habit:
  a preset menu at the cursor; Windows uses `TrackPopupMenu`, X11 cycles
  because there is no native popup without GTK.
- [fran-f/keypirinha-resize](https://github.com/fran-f/keypirinha-resize) —
  size presets as fractions of the work area; shared with Sizer.
- [FrancescElies/virgo](https://github.com/FrancescElies/virgo) — virtual
  desktops. Habit: a fixed row of numbered desktops; missing ones are created
  on demand, bound to `win+alt+1..9` and `win+shift+1..9`; `win+`` /
  `win+shift+`` cycle adjacent desktops.
- [fenchai23/QuickSwitch](https://github.com/fenchai23/QuickSwitch) — file
  dialogs. Habit: a Save/Open dialog lands in the last file manager's folder,
  no menu, suggested filename untouched. Deviation: the manual `fileDialog`
  combo is re-injected into the foreground app when no dialog is in front, so
  an app's own accelerator (DoubleCmd's ctrl+alt+l "list view") still works.
- [ArturTkaczuk/toggle-screen-autohotkey](https://github.com/ArturTkaczuk/toggle-screen-autohotkey)
  — monitor power. Habit: Win+F2 toggles; while "off" the power-off is
  re-asserted every second so stray input re-darkens the screens. Deviation:
  before powering off we snapshot every window's monitor + rect and restore
  the stranded ones on wake, because a DisplayPort monitor dropping its link
  in standby makes Windows consolidate its windows onto the primary monitor
  and never move them back.
- [microsoft/PowerToys](https://github.com/microsoft/PowerToys) —
  implementation reference for the low-level mouse hook (GrabAndMove), not a
  UX borrow.

## Behavior notes

- The daemon reads `desktop-tools.toml` next to the exe; a save reloads within
  ~1 s. `resize.toml` is re-read on every Sizer press.
- Windows are moved with `SetWindowPos` / X11 `ConfigureWindow`; DWM
  invisible borders are compensated so edges align. The OS snap and
  RectangleWin's FancyZones check skip WezTerm and QuteBrowser; we do not.
- Elevated windows need the daemon to run elevated; their file dialogs are
  left alone with a warning.
- Windows 10 1809–22H2 uses the undocumented `IVirtualDesktopManagerInternal`
  vtable (switch, create, move via `IApplicationViewCollection::GetViewForHwnd`
  + `MoveViewToDesktop`). Windows 11 has a different IID and reports
  "unavailable" rather than risk a wrong vtable.
- X11 depends on the WM honoring EWMH (`_NET_CURRENT_DESKTOP`,
  `_NET_WM_DESKTOP`, `_NET_WORKAREA`, `_NET_ACTIVE_WINDOW`); Wayland cannot
  move other apps' windows by design.
- `win+f2` posts `SC_MONITORPOWER` to Progman. A monitor that unplugs itself
  in standby (DisplayPort link drop) is detected by the monitor count and its
  windows are restored from a pre-power-off snapshot, matched by work area.
- Legacy `SysListView32` dialogs may need the file-name-box fallback; the
  modern path is verified on Notepad Save As and Sioyek Open.

## Deliberately not planned

Settings GUI, YAML config, URL/gist config import, autoupdate, installer
packaging, `pushTo*`, `makeSmaller/Larger`, `makeFullHeight`, `almostMaximize`,
`preview`/`preview_ms`, Linux tray (needs GTK/AppIndicator), Windows 11
virtual desktops (until needed). Add only on request.

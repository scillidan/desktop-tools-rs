//! Platform abstraction + the shared snapping state machine.
//!
//! `App` holds the cycle counters and drives a [`Platform`] implementation;
//! win32.rs and x11.rs only supply raw window/monitor/hotkey primitives.

use crate::config::Action;
use crate::layout::{
    center, inset, merge, scale_to, to_bottom, to_left, to_right, to_top, Rect, SizeSpec,
};
use anyhow::{Context, Result};

/// Opaque window handle: HWND value on Windows, X window id on X11.
pub type WinId = u64;

#[derive(Clone, Copy)]
pub struct Monitor {
    pub work: Rect,
}

/// Result of a backend's native Sizer preset picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(windows), allow(dead_code))] // X11 only constructs `Unsupported`.
pub enum SizerPick {
    Picked(usize),
    Cancelled,
    /// No native popup: the caller cycles the presets on repeated presses.
    Unsupported,
}

pub trait Platform {
    /// Work areas of all monitors, in enumeration order.
    fn monitors(&self) -> Vec<Monitor>;
    /// Index into `monitors()` for the monitor nearest `id`.
    fn monitor_index(&self, id: WinId) -> usize;
    fn foreground(&self) -> Option<WinId>;
    /// Last zonable foreground window remembered by a poll thread (Windows).
    fn last_active(&self) -> Option<WinId> {
        None
    }
    fn is_zonable(&self, id: WinId) -> bool;
    /// Full window rect, including invisible resize borders.
    fn window_rect(&self, id: WinId) -> Option<Rect>;
    /// Visible frame rect (DWM extended frame bounds on Windows).
    fn visible_frame(&self, id: WinId) -> Option<Rect>;
    fn move_window(&self, id: WinId, r: Rect) -> Result<()>;
    fn is_maximized(&self, id: WinId) -> bool;
    fn set_maximized(&self, id: WinId, on: bool) -> Result<()>;
    fn is_always_on_top(&self, id: WinId) -> bool;
    fn set_always_on_top(&self, id: WinId, on: bool) -> Result<()>;
    /// Keep the window at the bottom of the Z order (counterpart of topmost).
    /// Windows has no native flag; that backend re-asserts it on its 1 s tick.
    fn is_always_on_bottom(&self, _id: WinId) -> bool {
        false
    }
    fn set_always_on_bottom(&self, _id: WinId, _on: bool) -> Result<()> {
        anyhow::bail!("always-on-bottom is not supported on this platform")
    }
    fn minimize(&self, id: WinId) -> Result<()>;
    /// Re-show a minimized window (SW_RESTORE / X11 MapWindow).
    fn restore(&self, id: WinId) -> Result<()>;
    /// Clear the topmost flag from every window (DeskPins "unpin all").
    fn unpin_all(&self) -> Result<()>;
    /// Visible windows of the same app as `id`, top of the Z-order first.
    /// "Same app" is same process; Windows falls back to the executable
    /// image name so multi-process apps (each WezTerm window is its own
    /// wezterm-gui.exe) still cycle.
    fn same_app_windows(&self, id: WinId) -> Vec<WinId> {
        vec![id]
    }
    /// Bring `id` to the foreground.
    fn focus(&self, id: WinId) -> Result<()>;

    /// Switch to virtual desktop `n` (1-based).
    fn switch_desktop(&self, _n: u8) -> Result<()> {
        anyhow::bail!("virtual desktops are not supported on this platform")
    }

    /// Move a window to virtual desktop `n` (1-based) without switching.
    fn move_to_desktop(&self, _id: WinId, _n: u8) -> Result<()> {
        anyhow::bail!("virtual desktops are not supported on this platform")
    }

    /// Switch to the next virtual desktop, wrapping around the ends.
    fn next_desktop(&self) -> Result<()> {
        anyhow::bail!("virtual desktops are not supported on this platform")
    }

    /// Switch to the previous virtual desktop, wrapping around the ends.
    fn prev_desktop(&self) -> Result<()> {
        anyhow::bail!("virtual desktops are not supported on this platform")
    }

    /// Power all monitors off/on (the `toggleScreen` action).
    fn toggle_screen(&self) -> Result<()> {
        anyhow::bail!("toggling the monitor power is not supported on this platform")
    }
    /// Apply the remembered file-manager folder to the file dialog in front.
    fn file_dialog(&self) -> Result<()> {
        anyhow::bail!("file dialogs are not supported on this platform")
    }
    /// Show a preset picker at the cursor. `Unsupported` means the backend
    /// has no native popup and the caller should cycle on repeated presses.
    fn pick_sizer(&self, _presets: &[SizeSpec]) -> SizerPick {
        SizerPick::Unsupported
    }
    /// Main window of `pid`, if it is zonable.
    fn find_by_pid(&self, _pid: u32) -> Option<WinId> {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Edge,
    Corner,
}

/// One history record for the multi-level `undo` action.
#[derive(Clone, Copy)]
struct UndoEntry {
    id: WinId,
    /// Rect before the action (`None` for minimize: restore re-shows the
    /// window where it was, so no rect is needed).
    prev: Option<Rect>,
    minimized: bool,
}

/// Snapping state: only repeating one action on one window advances its cycle;
/// changing the window, direction, group, or resize action starts at one half.
pub struct App<P: Platform> {
    pub p: P,
    last_cycle: Option<(WinId, Group, usize)>,
    edge_turns: [u32; 4],
    corner_turns: [u32; 4],
    /// LIFO history of snaps/maximizes/minimizes for the `undo` action.
    undo: Vec<UndoEntry>,
    /// App windows snapshotted by the first `nextWindow` press. Focusing a
    /// window raises it, so re-querying Z-order would ping-pong two windows.
    cycle_windows: Vec<WinId>,
    cycle_idx: usize,
    sizer: Vec<SizeSpec>,
    /// Next preset used when the backend has no popup (X11).
    sizer_idx: usize,
    gap: i32,
}

/// Drop-oldest cap for the `undo` history.
const UNDO_CAP: usize = 32;

impl<P: Platform> App<P> {
    pub fn new(p: P) -> Self {
        App {
            p,
            last_cycle: None,
            edge_turns: [0; 4],
            corner_turns: [0; 4],
            undo: Vec::new(),
            cycle_windows: Vec::new(),
            cycle_idx: 0,
            sizer: Vec::new(),
            sizer_idx: 0,
            gap: 0,
        }
    }

    fn record_undo(&mut self, id: WinId, prev: Option<Rect>, minimized: bool) {
        if self.undo.len() == UNDO_CAP {
            self.undo.remove(0);
        }
        self.undo.push(UndoEntry {
            id,
            prev,
            minimized,
        });
    }

    pub fn set_gap(&mut self, gap: i32) {
        self.gap = gap.max(0);
    }

    pub fn set_sizer(&mut self, presets: Vec<SizeSpec>) {
        self.sizer = presets;
        self.sizer_idx = 0;
    }

    pub fn apply(&mut self, action: Action) -> Result<()> {
        self.apply_to(action, None)
    }

    /// `forced` overrides the foreground target (for `--pid <pid>` one-shots).
    pub fn apply_to(&mut self, action: Action, forced: Option<WinId>) -> Result<()> {
        match action {
            Action::MoveToLeft => self.cycle(Group::Edge, 0, forced),
            Action::MoveToRight => self.cycle(Group::Edge, 1, forced),
            Action::MoveToTop => self.cycle(Group::Edge, 2, forced),
            Action::MoveToBottom => self.cycle(Group::Edge, 3, forced),
            Action::MoveToTopLeft => self.cycle(Group::Corner, 0, forced),
            Action::MoveToTopRight => self.cycle(Group::Corner, 1, forced),
            Action::MoveToBottomLeft => self.cycle(Group::Corner, 2, forced),
            Action::MoveToBottomRight => self.cycle(Group::Corner, 3, forced),
            Action::MoveToCenter => {
                let t = self.target(forced)?;
                self.resize(t, center, 0)?;
                Ok(())
            }
            Action::Maximize => {
                self.last_cycle = None;
                let t = self.target(forced)?;
                if self.p.is_maximized(t) {
                    return self.p.set_maximized(t, false);
                }
                let before = self.p.window_rect(t);
                self.p.set_maximized(t, true)?;
                if let Some(before) = before {
                    self.record_undo(t, Some(before), false);
                }
                Ok(())
            }
            Action::NextDisplay => {
                let t = self.target(forced)?;
                self.resize(t, center, 1)?;
                Ok(())
            }
            Action::PrevDisplay => {
                let t = self.target(forced)?;
                self.resize(t, center, -1)?;
                Ok(())
            }
            Action::ToggleAlwaysOnTop => {
                let t = self.target(forced)?;
                let on = !self.p.is_always_on_top(t);
                self.p.set_always_on_top(t, on)
            }
            Action::ToggleAlwaysOnBottom => {
                let t = self.target(forced)?;
                let on = !self.p.is_always_on_bottom(t);
                self.p.set_always_on_bottom(t, on)
            }
            Action::Minimize => {
                self.last_cycle = None;
                let t = self.target(forced)?;
                self.record_undo(t, None, true);
                self.p.minimize(t)
            }
            Action::UnpinAll => self.p.unpin_all(),
            Action::NextWindow => self.switch_window(1, forced),
            Action::PrevWindow => self.switch_window(-1, forced),
            Action::SizerMenu => self.apply_sizer(forced),
            Action::Undo => {
                let Some(e) = self.undo.pop() else {
                    anyhow::bail!("nothing to undo");
                };
                self.last_cycle = None; // next snap starts a fresh cycle
                if e.minimized {
                    // A minimized window fails `is_zonable` by design, so only
                    // check the window still exists (rect query fails on death).
                    if self.p.window_rect(e.id).is_none() {
                        anyhow::bail!("that window is gone");
                    }
                    return self.p.restore(e.id);
                }
                if !self.p.is_zonable(e.id) {
                    anyhow::bail!("that window is gone");
                }
                let Some(prev) = e.prev else {
                    return Ok(());
                };
                self.p.move_window(e.id, prev)
            }
            Action::Quit => Ok(()),
            Action::FileDialog => self.p.file_dialog(),
            Action::SwitchToDesktop(n) => self.p.switch_desktop(n),
            Action::NextDesktop => self.p.next_desktop(),
            Action::PrevDesktop => self.p.prev_desktop(),
            Action::Suppress => Ok(()),
            Action::MoveToDesktop(n) => {
                let t = self.target(forced)?;
                self.p.move_to_desktop(t, n)
            }
            Action::ToggleScreen => self.p.toggle_screen(),
        }
    }

    /// Cycle to the next/previous visible window of the same process.
    fn switch_window(&mut self, dir: isize, forced: Option<WinId>) -> Result<()> {
        let fg = match forced {
            Some(w) if self.p.is_zonable(w) => w,
            _ => self
                .p
                .foreground()
                .filter(|&w| self.p.is_zonable(w))
                .or_else(|| self.p.last_active())
                .context("no foreground window")?,
        };
        if !self.cycle_windows.contains(&fg) {
            self.cycle_windows = self.p.same_app_windows(fg);
            self.cycle_idx = self
                .cycle_windows
                .iter()
                .position(|&w| w == fg)
                .unwrap_or(0);
        }
        let current = self.cycle_windows.get(self.cycle_idx).copied();
        self.cycle_windows.retain(|&w| self.p.is_zonable(w));
        if let Some(w) = current {
            if let Some(i) = self.cycle_windows.iter().position(|&x| x == w) {
                self.cycle_idx = i;
            }
        }
        anyhow::ensure!(
            !self.cycle_windows.is_empty(),
            "no windows to switch between"
        );
        let len = self.cycle_windows.len() as isize;
        self.cycle_idx = (self.cycle_idx as isize + dir).rem_euclid(len) as usize;
        self.p.focus(self.cycle_windows[self.cycle_idx])
    }

    /// Apply a Sizer preset. Windows shows a native popup; backends without
    /// one cycle the presets on repeated presses. Keep the visible top-left
    /// and adjust the outer rect for DWM's invisible borders.
    fn apply_sizer(&mut self, forced: Option<WinId>) -> Result<()> {
        anyhow::ensure!(
            !self.sizer.is_empty(),
            "no sizer presets; add entries to resize.toml next to desktop-tools.toml"
        );
        let idx = match self.p.pick_sizer(&self.sizer) {
            SizerPick::Picked(i) => i,
            SizerPick::Cancelled => return Ok(()),
            SizerPick::Unsupported => {
                let i = self.sizer_idx % self.sizer.len();
                self.sizer_idx = (i + 1) % self.sizer.len();
                i
            }
        };
        let spec = *self
            .sizer
            .get(idx)
            .context("sizer picker returned an invalid preset")?;
        let t = self.target(forced)?;
        let mons = self.p.monitors();
        anyhow::ensure!(!mons.is_empty(), "no monitors");
        let work = mons
            .get(self.p.monitor_index(t))
            .context("monitor index out of range")?
            .work;
        let (w, h) = spec.resolve(work);
        let rect = self.p.window_rect(t).context("cannot query window rect")?;
        let frame = self.p.visible_frame(t).unwrap_or(rect);
        let l_extra = frame.l - rect.l;
        let t_extra = frame.t - rect.t;
        let r_extra = rect.r - frame.r;
        let b_extra = rect.b - frame.b;
        let new_pos = Rect {
            l: rect.l,
            t: rect.t,
            r: rect.l + w + l_extra + r_extra,
            b: rect.t + h + t_extra + b_extra,
        };
        if new_pos == rect {
            return Ok(());
        }
        self.last_cycle = None;
        self.p.move_window(t, new_pos)?;
        self.record_undo(t, Some(rect), false);
        Ok(())
    }

    fn target(&self, forced: Option<WinId>) -> Result<WinId> {
        if let Some(w) = forced {
            if self.p.is_zonable(w) {
                return Ok(w);
            }
        }
        if let Some(w) = self.p.foreground() {
            if self.p.is_zonable(w) {
                return Ok(w);
            }
        }
        if let Some(w) = self.p.last_active() {
            if self.p.is_zonable(w) {
                return Ok(w);
            }
        }
        anyhow::bail!("no zonable target window in foreground")
    }

    /// Half -> two thirds -> one third cycle, RectangleWin semantics.
    fn cycle(&mut self, group: Group, i: usize, forced: Option<WinId>) -> Result<()> {
        let hwnd = self.target(forced)?;
        if self.last_cycle != Some((hwnd, group, i)) {
            self.edge_turns = [0; 4];
            self.corner_turns = [0; 4];
        }
        let turns = match group {
            Group::Edge => &mut self.edge_turns,
            Group::Corner => &mut self.corner_turns,
        };
        let (mul, div) = [(1, 2), (2, 3), (1, 3)][(turns[i] % 3) as usize];
        turns[i] += 1;
        for (j, t) in turns.iter_mut().enumerate() {
            if j != i {
                *t = 0;
            }
        }
        // Corners cycle width only; height stays half (RectangleWin merge).
        let f: Box<dyn Fn(Rect) -> Rect> = match (group, i) {
            (Group::Edge, 0) => Box::new(move |w| to_left(w, mul, div)),
            (Group::Edge, 1) => Box::new(move |w| to_right(w, mul, div)),
            (Group::Edge, 2) => Box::new(move |w| to_top(w, mul, div)),
            (Group::Edge, 3) => Box::new(move |w| to_bottom(w, mul, div)),
            (Group::Corner, 0) => Box::new(move |w| merge(to_left(w, mul, div), to_top(w, 1, 2))),
            (Group::Corner, 1) => Box::new(move |w| merge(to_right(w, mul, div), to_top(w, 1, 2))),
            (Group::Corner, 2) => {
                Box::new(move |w| merge(to_left(w, mul, div), to_bottom(w, 1, 2)))
            }
            (Group::Corner, 3) => {
                Box::new(move |w| merge(to_right(w, mul, div), to_bottom(w, 1, 2)))
            }
            _ => unreachable!(),
        };
        self.resize(hwnd, |work, _| f(work), 0)?;
        self.last_cycle = Some((hwnd, group, i));
        Ok(())
    }

    /// Port of RectangleWin `resize` + `resizeAcrossMonitor`: work-area snap
    /// with invisible-border compensation; `monitor_diff` moves across monitors
    /// (window keeps size, is re-centered on the target work area).
    fn resize(
        &mut self,
        hwnd: WinId,
        f: impl Fn(Rect, Rect) -> Rect,
        monitor_diff: i32,
    ) -> Result<Rect> {
        let rect = self
            .p
            .window_rect(hwnd)
            .context("cannot query window rect")?;
        let mons = self.p.monitors();
        anyhow::ensure!(!mons.is_empty(), "no monitors");
        let cur = self.p.monitor_index(hwnd);
        let idx = if monitor_diff != 0 {
            (cur as i32 + monitor_diff).rem_euclid(mons.len() as i32) as usize
        } else {
            cur
        };
        let work = inset(mons[idx].work, self.gap);

        let frame = self.p.visible_frame(hwnd).unwrap_or(rect);
        let l_extra = frame.l - rect.l;
        let r_extra = rect.r - frame.r;
        let t_extra = frame.t - rect.t;
        let b_extra = rect.b - frame.b;

        // Across monitors keep the same fraction of the screen, not the same
        // pixels: screens differ in size.
        let frame = if monitor_diff != 0 {
            scale_to(frame, inset(mons[cur].work, self.gap), work)
        } else {
            frame
        };

        let mut new_pos = f(work, frame);
        new_pos.l -= l_extra;
        new_pos.t -= t_extra;
        new_pos.r += r_extra;
        new_pos.b += b_extra;

        self.last_cycle = None;
        if new_pos == rect {
            return Ok(new_pos);
        }
        self.p.move_window(hwnd, new_pos)?;
        self.record_undo(hwnd, Some(rect), false);
        Ok(new_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory platform: one 1000x700 work area at (0,0), a zonable window
    /// whose rect updates on move so cycle progression is observable.
    struct Mock {
        rect: std::cell::RefCell<Rect>,
        monitors: Vec<Monitor>,
        maximized: std::cell::Cell<bool>,
        minimized: std::cell::RefCell<std::collections::HashSet<WinId>>,
        aot: std::cell::Cell<bool>,
        foreground: std::cell::Cell<WinId>,
        focused: std::cell::Cell<WinId>,
        app_windows: std::cell::RefCell<Vec<WinId>>,
    }

    impl Platform for Mock {
        fn monitors(&self) -> Vec<Monitor> {
            self.monitors.clone()
        }
        fn monitor_index(&self, _id: WinId) -> usize {
            0
        }
        fn foreground(&self) -> Option<WinId> {
            let id = self.foreground.get();
            (id != 0).then_some(id)
        }
        fn is_zonable(&self, _id: WinId) -> bool {
            true
        }
        fn window_rect(&self, _id: WinId) -> Option<Rect> {
            Some(*self.rect.borrow())
        }
        fn visible_frame(&self, _id: WinId) -> Option<Rect> {
            Some(*self.rect.borrow())
        }
        fn move_window(&self, _id: WinId, r: Rect) -> Result<()> {
            *self.rect.borrow_mut() = r;
            Ok(())
        }
        fn is_maximized(&self, _id: WinId) -> bool {
            self.maximized.get()
        }
        fn set_maximized(&self, _id: WinId, on: bool) -> Result<()> {
            self.maximized.set(on);
            Ok(())
        }
        fn is_always_on_top(&self, _id: WinId) -> bool {
            self.aot.get()
        }
        fn set_always_on_top(&self, _id: WinId, on: bool) -> Result<()> {
            self.aot.set(on);
            Ok(())
        }
        fn minimize(&self, id: WinId) -> Result<()> {
            self.minimized.borrow_mut().insert(id);
            Ok(())
        }
        fn restore(&self, id: WinId) -> Result<()> {
            self.minimized.borrow_mut().remove(&id);
            Ok(())
        }
        fn unpin_all(&self) -> Result<()> {
            self.aot.set(false);
            Ok(())
        }
        fn same_app_windows(&self, _id: WinId) -> Vec<WinId> {
            self.app_windows.borrow().clone()
        }
        fn focus(&self, id: WinId) -> Result<()> {
            self.focused.set(id);
            self.foreground.set(id);
            Ok(())
        }
    }

    fn mock() -> Mock {
        Mock {
            rect: std::cell::RefCell::new(Rect {
                l: 10,
                t: 10,
                r: 610,
                b: 410,
            }),
            monitors: vec![Monitor {
                work: Rect {
                    l: 0,
                    t: 0,
                    r: 1000,
                    b: 700,
                },
            }],
            maximized: std::cell::Cell::new(false),
            minimized: std::cell::RefCell::new(std::collections::HashSet::new()),
            aot: std::cell::Cell::new(false),
            foreground: std::cell::Cell::new(1),
            focused: std::cell::Cell::new(1),
            app_windows: std::cell::RefCell::new(vec![1]),
        }
    }

    #[test]
    fn cycle_progression_and_wrap() {
        let mut app = App::new(mock());
        // Same window, repeated presses: 1/2 -> 2/3 -> 1/3 -> back to 1/2.
        for expected in [500, 667, 334, 500] {
            app.apply(Action::MoveToLeft).unwrap();
            assert_eq!(app.p.window_rect(1).unwrap().w(), expected, "cycle width");
        }
        // A different direction resets the left counter: right = half again.
        app.apply(Action::MoveToRight).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().r, 1000);
        app.apply(Action::MoveToLeft).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);
    }

    #[test]
    fn changing_between_edges_and_corners_restarts_at_half() {
        let mut app = App::new(mock());
        app.apply(Action::MoveToLeft).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);

        app.apply(Action::MoveToBottomRight).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);
        app.apply(Action::MoveToBottomRight).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 667);

        app.apply(Action::MoveToLeft).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);
    }

    #[test]
    fn corner_cycles_horizontal_only() {
        let mut app = App::new(mock());
        app.apply(Action::MoveToTopLeft).unwrap();
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 0,
                t: 0,
                r: 500,
                b: 350
            }
        );
        app.apply(Action::MoveToTopLeft).unwrap(); // two thirds wide
        assert_eq!(app.p.window_rect(1).unwrap().r, 667);
    }

    #[test]
    fn maximize_toggles() {
        let mut app = App::new(mock());
        app.apply(Action::Maximize).unwrap();
        assert!(app.p.is_maximized(1));
        app.apply(Action::Maximize).unwrap();
        assert!(!app.p.is_maximized(1));
    }

    #[test]
    fn minimize_minimizes_the_target() {
        let mut app = App::new(mock());
        app.apply(Action::Minimize).unwrap();
        assert!(app.p.minimized.borrow().contains(&1));
    }

    #[test]
    fn unpin_all_clears_topmost() {
        let mut app = App::new(mock());
        app.p.aot.set(true);
        app.apply(Action::UnpinAll).unwrap();
        assert!(!app.p.aot.get());
    }

    #[test]
    fn next_window_cycles_the_foreground_app() {
        let mut app = App::new(mock());
        *app.p.app_windows.borrow_mut() = vec![1, 2, 3];

        app.apply(Action::NextWindow).unwrap();
        assert_eq!(app.p.focused.get(), 2);
        app.apply(Action::NextWindow).unwrap();
        assert_eq!(app.p.focused.get(), 3);
        app.apply(Action::NextWindow).unwrap();
        assert_eq!(app.p.focused.get(), 1);
        app.apply(Action::PrevWindow).unwrap();
        assert_eq!(app.p.focused.get(), 3);

        // Another app starts a fresh cycle instead of reusing the old list.
        app.p.foreground.set(9);
        *app.p.app_windows.borrow_mut() = vec![9, 10];
        app.apply(Action::PrevWindow).unwrap();
        assert_eq!(app.p.focused.get(), 10);
    }

    #[test]
    fn sizer_cycles_presets_without_a_popup() {
        let mut app = App::new(mock());
        app.set_sizer(vec!["500x300".parse().unwrap(), "1/2".parse().unwrap()]);

        app.apply(Action::SizerMenu).unwrap();
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 10,
                t: 10,
                r: 510,
                b: 310
            }
        );
        app.apply(Action::SizerMenu).unwrap();
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 10,
                t: 10,
                r: 510,
                b: 360
            }
        );
        // Past the last preset: wrap to the first.
        app.apply(Action::SizerMenu).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().h(), 300);
    }

    #[test]
    fn next_display_scales_to_the_target_monitor() {
        let mut app = App::new(mock());
        app.p.monitors.push(Monitor {
            work: Rect {
                l: 1000,
                t: 0,
                r: 3000,
                b: 1050,
            },
        });
        app.apply(Action::NextDisplay).unwrap();
        // 600x400 window at the 1000x700 ratio, scaled 1.5x and centered.
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 1550,
                t: 225,
                r: 2450,
                b: 825
            }
        );
    }

    #[test]
    fn other_resize_actions_restart_the_snap_cycle() {
        for interrupt in [
            Action::MoveToCenter,
            Action::NextDisplay,
            Action::SizerMenu,
            Action::Maximize,
            Action::Minimize,
        ] {
            let mut app = App::new(mock());
            app.set_sizer(vec![crate::layout::SizeSpec::Px { w: 800, h: 600 }]);
            app.apply(Action::MoveToLeft).unwrap();
            app.apply(Action::MoveToLeft).unwrap();
            assert_eq!(app.p.window_rect(1).unwrap().w(), 667);
            app.apply(interrupt).unwrap();
            app.apply(Action::MoveToLeft).unwrap();
            assert_eq!(
                app.p.window_rect(1).unwrap().w(),
                500,
                "after {interrupt:?}"
            );
        }
    }

    #[test]
    fn undo_walks_back_the_history() {
        let mut app = App::new(mock());
        let before = app.p.window_rect(1).unwrap();

        app.apply(Action::MoveToLeft).unwrap();
        let half = app.p.window_rect(1).unwrap();
        app.apply(Action::MoveToLeft).unwrap();
        let two_thirds = app.p.window_rect(1).unwrap();
        assert_eq!(two_thirds.w(), 667);

        // Multi-level: each undo restores the previous step.
        app.apply(Action::Undo).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap(), half);
        app.apply(Action::Undo).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap(), before);

        // History exhausted: a further undo reports it instead of a silent
        // no-op, so the hotkey never looks dead.
        assert!(app.apply(Action::Undo).is_err());
        assert_eq!(app.p.window_rect(1).unwrap(), before);

        // Undo reset the cycle counter, so left snaps to one half again.
        app.apply(Action::MoveToLeft).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);
    }

    #[test]
    fn undo_restores_minimized_windows_lifo() {
        let mut app = App::new(mock());
        app.apply_to(Action::Minimize, Some(1)).unwrap();
        app.apply_to(Action::Minimize, Some(2)).unwrap();
        assert_eq!(app.p.minimized.borrow().len(), 2);

        // Last minimized comes back first.
        app.apply(Action::Undo).unwrap();
        assert!(!app.p.minimized.borrow().contains(&2));
        assert!(app.p.minimized.borrow().contains(&1));
        app.apply(Action::Undo).unwrap();
        assert!(app.p.minimized.borrow().is_empty());
    }

    #[test]
    fn undo_snaps_and_minimize_share_one_history() {
        let mut app = App::new(mock());
        let before = app.p.window_rect(1).unwrap();
        app.apply(Action::MoveToLeft).unwrap();
        app.apply_to(Action::Minimize, Some(1)).unwrap();

        // First undo re-shows the window (still at its snapped rect),
        // second undo moves it back to the pre-snap rect.
        app.apply(Action::Undo).unwrap();
        assert!(!app.p.minimized.borrow().contains(&1));
        assert_eq!(app.p.window_rect(1).unwrap().w(), 500);
        app.apply(Action::Undo).unwrap();
        assert_eq!(app.p.window_rect(1).unwrap(), before);
    }

    #[test]
    fn gap_insets_the_work_area() {
        let mut app = App::new(mock());
        app.set_gap(20);
        app.apply(Action::MoveToLeft).unwrap();
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 20,
                t: 20,
                r: 500,
                b: 680
            }
        );
        app.apply(Action::MoveToBottomRight).unwrap();
        assert_eq!(
            app.p.window_rect(1).unwrap(),
            Rect {
                l: 500,
                t: 350,
                r: 980,
                b: 680
            }
        );
    }

    #[test]
    fn no_target_window_errors() {
        struct NoTarget;
        impl Platform for NoTarget {
            fn monitors(&self) -> Vec<Monitor> {
                vec![Monitor {
                    work: Rect {
                        l: 0,
                        t: 0,
                        r: 10,
                        b: 10,
                    },
                }]
            }
            fn monitor_index(&self, _id: WinId) -> usize {
                0
            }
            fn foreground(&self) -> Option<WinId> {
                None
            }
            fn is_zonable(&self, _id: WinId) -> bool {
                false
            }
            fn window_rect(&self, _id: WinId) -> Option<Rect> {
                None
            }
            fn visible_frame(&self, _id: WinId) -> Option<Rect> {
                None
            }
            fn move_window(&self, _id: WinId, _r: Rect) -> Result<()> {
                Ok(())
            }
            fn is_maximized(&self, _id: WinId) -> bool {
                false
            }
            fn set_maximized(&self, _id: WinId, _on: bool) -> Result<()> {
                Ok(())
            }
            fn is_always_on_top(&self, _id: WinId) -> bool {
                false
            }
            fn set_always_on_top(&self, _id: WinId, _on: bool) -> Result<()> {
                Ok(())
            }
            fn minimize(&self, _id: WinId) -> Result<()> {
                Ok(())
            }
            fn restore(&self, _id: WinId) -> Result<()> {
                Ok(())
            }
            fn unpin_all(&self) -> Result<()> {
                Ok(())
            }
            fn focus(&self, _id: WinId) -> Result<()> {
                Ok(())
            }
        }
        let mut app = App::new(NoTarget);
        assert!(app.apply(Action::MoveToLeft).is_err());
    }
}

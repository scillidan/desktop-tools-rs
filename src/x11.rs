//! Linux X11 backend (x11rb, pure Rust — no libxcb build dependency).
//!
//! Wayland note: Wayland compositors deliberately do not let clients move
//! other apps' windows, so this backend speaks X11 only. Under XWayland it
//! manages X11 clients; native Wayland windows are out of scope by design.

use crate::config::Binding;
use crate::keys::keysym_of;
use crate::layout::{intersect, Rect};
use crate::platform::{Monitor, Platform, WinId};
use anyhow::{Context, Result};
use std::collections::HashMap;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::protocol::xproto::{self, ConnectionExt as XExt};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

#[derive(Clone, Copy)]
struct Atoms {
    active_window: u32,
    client_list: u32,
    wm_state: u32,
    state_max_v: u32,
    state_max_h: u32,
    state_above: u32,
    wm_change_state: u32,
    wm_window_type: u32,
    type_desktop: u32,
    type_dock: u32,
    workarea: u32,
    wm_pid: u32,
    wm_class: u32,
    current_desktop: u32,
    wm_desktop: u32,
}

fn intern(conn: &RustConnection, name: &[u8]) -> Result<u32> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

fn atoms(conn: &RustConnection) -> Result<Atoms> {
    Ok(Atoms {
        active_window: intern(conn, b"_NET_ACTIVE_WINDOW")?,
        client_list: intern(conn, b"_NET_CLIENT_LIST")?,
        wm_state: intern(conn, b"_NET_WM_STATE")?,
        state_max_v: intern(conn, b"_NET_WM_STATE_MAXIMIZED_VERT")?,
        state_max_h: intern(conn, b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
        state_above: intern(conn, b"_NET_WM_STATE_ABOVE")?,
        wm_change_state: intern(conn, b"WM_CHANGE_STATE")?,
        wm_window_type: intern(conn, b"_NET_WM_WINDOW_TYPE")?,
        type_desktop: intern(conn, b"_NET_WM_WINDOW_TYPE_DESKTOP")?,
        type_dock: intern(conn, b"_NET_WM_WINDOW_TYPE_DOCK")?,
        workarea: intern(conn, b"_NET_WORKAREA")?,
        wm_pid: intern(conn, b"_NET_WM_PID")?,
        wm_class: intern(conn, b"WM_CLASS")?,
        current_desktop: intern(conn, b"_NET_CURRENT_DESKTOP")?,
        wm_desktop: intern(conn, b"_NET_WM_DESKTOP")?,
    })
}

/// Keysym -> keycode map from the server keyboard mapping.
fn build_keymap(conn: &RustConnection) -> Result<HashMap<u32, u8>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let reply = conn.get_keyboard_mapping(min, count)?.reply()?;
    let per = reply.keysyms_per_keycode.max(1) as usize;
    let mut map = HashMap::new();
    for (i, sym) in reply.keysyms.iter().enumerate() {
        let kc = min + (i / per) as u8;
        map.entry(*sym).or_insert(kc);
    }
    Ok(map)
}

fn mod_mask(c: &crate::keys::Combo) -> u16 {
    let mut m = 0u16;
    if c.ctrl {
        m |= u16::from(xproto::ModMask::CONTROL);
    }
    if c.shift {
        m |= u16::from(xproto::ModMask::SHIFT);
    }
    if c.alt {
        m |= u16::from(xproto::ModMask::M1);
    }
    if c.win {
        m |= u16::from(xproto::ModMask::M4);
    }
    m
}

fn lock_mask() -> u16 {
    u16::from(xproto::ModMask::LOCK)
}

fn mod2_mask() -> u16 {
    u16::from(xproto::ModMask::M2)
}

pub struct X11 {
    conn: RustConnection,
    root: u32,
    screen: usize,
    a: Atoms,
    /// (keycode, mods with Lock/Mod2 stripped) -> binding index.
    grabs: HashMap<(u8, u16), usize>,
}

pub fn connect() -> Result<X11> {
    let (conn, screen) = x11rb::connect(None).context(
        "cannot connect to the X server (set DISPLAY; Wayland-only sessions cannot move windows)",
    )?;
    let root = conn.setup().roots[screen].root;
    let a = atoms(&conn)?;
    Ok(X11 {
        conn,
        root,
        screen,
        a,
        grabs: HashMap::new(),
    })
}

impl X11 {
    /// Grab every binding; returns combos that could not be grabbed.
    pub fn grab_bindings(&mut self, bindings: &[Binding]) -> Vec<String> {
        let keymap = build_keymap(&self.conn).unwrap_or_default();
        let mut failed = Vec::new();
        for (i, b) in bindings.iter().enumerate() {
            let Some(sym) = keysym_of(b.combo.key) else {
                failed.push(format!("{} (unknown key)", b.combo));
                continue;
            };
            let Some(&kc) = keymap.get(&sym) else {
                failed.push(format!("{} (key not in keyboard map)", b.combo));
                continue;
            };
            let mods = mod_mask(&b.combo);
            // Grab with LockMask/Mod2Mask variants so CapsLock/NumLock do not
            // defeat the grab; strip them again when matching events.
            let mut ok = true;
            for extra in [0u16, lock_mask(), mod2_mask()] {
                let r = self.conn.grab_key(
                    false,
                    self.root,
                    (mods | extra).into(),
                    kc,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                );
                ok &= match r {
                    Ok(c) => c.check().is_ok(),
                    Err(_) => false,
                };
            }
            if ok {
                self.grabs.insert((kc, mods), i);
            } else {
                failed.push(format!("{} (already grabbed by another client)", b.combo));
            }
        }
        let _ = self.conn.flush();
        failed
    }

    fn prop32(&self, win: u32, atom: u32) -> Option<Vec<u32>> {
        let reply = self
            .conn
            .get_property(false, win, atom, 0u32, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        let values: Vec<u32> = reply.value32()?.collect();
        Some(values)
    }

    /// `WM_CLASS` bytes (two NUL-terminated strings), used when a window does
    /// not advertise `_NET_WM_PID`.
    fn class_of(&self, win: u32) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, win, self.a.wm_class, 0u32, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        (!reply.value.is_empty()).then_some(reply.value)
    }

    fn rect(&self, win: u32) -> Option<Rect> {
        let geo = self.conn.get_geometry(win).ok()?.reply().ok()?;
        let pos = self
            .conn
            .translate_coordinates(win, self.root, geo.x, geo.y)
            .ok()?
            .reply()
            .ok()?;
        Some(Rect {
            l: pos.dst_x as i32,
            t: pos.dst_y as i32,
            r: pos.dst_x as i32 + geo.width as i32,
            b: pos.dst_y as i32 + geo.height as i32,
        })
    }

    /// EWMH client message to the root window (the WM applies it).
    fn send_client_message(&self, window: u32, type_: u32, data: [u32; 5]) -> Result<()> {
        let ev = xproto::ClientMessageEvent {
            response_type: xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window,
            type_,
            data: data.into(),
        };
        self.conn
            .send_event(
                false,
                self.root,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            )?
            .check()?;
        Ok(())
    }

    /// `_NET_WM_STATE` client message (remove = 0, add = 1) to the root window.
    fn send_wm_state(&self, win: u32, mode: u32, a1: u32, a2: u32) -> Result<()> {
        self.send_client_message(win, self.a.wm_state, [mode, a1, a2, 1, 0])
    }

    /// Non-blocking: next grabbed hotkey already in the event queue, if any.
    /// The daemon polls this so it can also watch the config file.
    pub fn poll_hotkey(&self) -> Result<Option<usize>> {
        while let Some(ev) = self.conn.poll_for_event()? {
            if let Event::KeyPress(e) = ev {
                let mods = u16::from(e.state) & !(lock_mask() | mod2_mask());
                if let Some(&i) = self.grabs.get(&(e.detail, mods)) {
                    return Ok(Some(i));
                }
            }
        }
        Ok(None)
    }

    /// Release every grab (config reload / suspend).
    pub fn ungrab_all(&mut self) {
        for &(kc, mods) in self.grabs.keys() {
            for extra in [0u16, lock_mask(), mod2_mask()] {
                let _ = self.conn.ungrab_key(kc, self.root, (mods | extra).into());
            }
        }
        self.grabs.clear();
        let _ = self.conn.flush();
    }

    pub fn grab_count(&self) -> usize {
        self.grabs.len()
    }
}

impl Platform for X11 {
    fn monitors(&self) -> Vec<Monitor> {
        let mut rects: Vec<Rect> = Vec::new();
        if self
            .conn
            .extension_information("RANDR")
            .ok()
            .flatten()
            .is_some()
        {
            if let Ok(res) = self.conn.randr_get_screen_resources(self.root) {
                if let Ok(res) = res.reply() {
                    for crtc in res.crtcs {
                        if let Ok(info) = self.conn.randr_get_crtc_info(crtc, CURRENT_TIME) {
                            if let Ok(info) = info.reply() {
                                if info.mode != 0 && info.width > 0 && info.height > 0 {
                                    rects.push(Rect {
                                        l: info.x as i32,
                                        t: info.y as i32,
                                        r: info.x as i32 + info.width as i32,
                                        b: info.y as i32 + info.height as i32,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        if rects.is_empty() {
            let s = &self.conn.setup().roots[self.screen];
            rects.push(Rect {
                l: 0,
                t: 0,
                r: s.width_in_pixels as i32,
                b: s.height_in_pixels as i32,
            });
        }
        // Work area (panels/struts): intersect with _NET_WORKAREA if present.
        if let Some(v) = self.prop32(self.root, self.a.workarea) {
            if v.len() >= 4 {
                let wa = Rect {
                    l: v[0] as i32,
                    t: v[1] as i32,
                    r: v[0] as i32 + v[2] as i32,
                    b: v[1] as i32 + v[3] as i32,
                };
                rects = rects
                    .iter()
                    .map(|m| intersect(*m, wa))
                    .filter(|m| m.has_area())
                    .collect();
            }
        }
        rects.into_iter().map(|work| Monitor { work }).collect()
    }

    fn monitor_index(&self, id: WinId) -> usize {
        let mons = self.monitors();
        if mons.is_empty() {
            return 0;
        }
        let Some(r) = self.rect(id as u32) else {
            return 0;
        };
        let (cx, cy) = ((r.l + r.r) / 2, (r.t + r.b) / 2);
        mons.iter()
            .position(|m| {
                let w = m.work;
                cx >= w.l && cx < w.r && cy >= w.t && cy < w.b
            })
            .unwrap_or(0)
    }

    fn foreground(&self) -> Option<WinId> {
        if let Some(v) = self.prop32(self.root, self.a.active_window) {
            if let Some(&w) = v.first() {
                if w != 0 && w != self.root {
                    return Some(w as u64);
                }
            }
        }
        // Fallback: the input focus window.
        if let Ok(r) = self.conn.get_input_focus() {
            if let Ok(r) = r.reply() {
                if r.focus != self.root && r.focus != 0 {
                    return Some(r.focus as u64);
                }
            }
        }
        None
    }

    fn is_zonable(&self, id: WinId) -> bool {
        let win = id as u32;
        if win == 0 || win == self.root {
            return false;
        }
        let Ok(attrs) = self.conn.get_window_attributes(win) else {
            return false;
        };
        let Ok(attrs) = attrs.reply() else {
            return false;
        };
        if attrs.map_state != xproto::MapState::VIEWABLE || attrs.override_redirect {
            return false;
        }
        if let Some(types) = self.prop32(win, self.a.wm_window_type) {
            if types.contains(&self.a.type_desktop) || types.contains(&self.a.type_dock) {
                return false;
            }
        }
        self.rect(win).map(|r| r.has_area()).unwrap_or(false)
    }

    fn window_rect(&self, id: WinId) -> Option<Rect> {
        self.rect(id as u32)
    }

    fn visible_frame(&self, id: WinId) -> Option<Rect> {
        self.rect(id as u32)
    }

    fn move_window(&self, id: WinId, r: Rect) -> Result<()> {
        let win = id as u32;
        if self.is_maximized(id) {
            self.send_wm_state(win, 0, self.a.state_max_v, self.a.state_max_h)?;
        }
        let aux = xproto::ConfigureWindowAux::new()
            .x(r.l)
            .y(r.t)
            .width(r.w() as u32)
            .height(r.h() as u32);
        self.conn.configure_window(win, &aux)?.check()?;
        self.conn.flush()?;
        Ok(())
    }

    fn is_maximized(&self, id: WinId) -> bool {
        self.prop32(id as u32, self.a.wm_state)
            .map(|states| {
                states.contains(&self.a.state_max_v) || states.contains(&self.a.state_max_h)
            })
            .unwrap_or(false)
    }

    fn set_maximized(&self, id: WinId, on: bool) -> Result<()> {
        self.send_wm_state(
            id as u32,
            if on { 1 } else { 0 },
            self.a.state_max_v,
            self.a.state_max_h,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    fn is_always_on_top(&self, id: WinId) -> bool {
        self.prop32(id as u32, self.a.wm_state)
            .map(|s| s.contains(&self.a.state_above))
            .unwrap_or(false)
    }

    fn set_always_on_top(&self, id: WinId, on: bool) -> Result<()> {
        // mode 1 = add, 0 = remove (toggle via _NET_WM_STATE message is mode 2,
        // but being explicit avoids a race with is_always_on_top).
        self.send_wm_state(id as u32, if on { 1 } else { 0 }, self.a.state_above, 0)?;
        self.conn.flush()?;
        Ok(())
    }

    fn minimize(&self, id: WinId) -> Result<()> {
        // No core IconifyWindow request exists; XIconifyWindow is exactly this
        // WM_CHANGE_STATE client message (3 = IconicState).
        let data = xproto::ClientMessageData::from([3u32, 0, 0, 0, 0]);
        let ev = xproto::ClientMessageEvent {
            response_type: xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: id as u32,
            type_: self.a.wm_change_state,
            data,
        };
        self.conn
            .send_event(
                false,
                self.root,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            )?
            .check()?;
        self.conn.flush()?;
        Ok(())
    }

    fn restore(&self, id: WinId) -> Result<()> {
        self.conn.map_window(id as u32)?.check()?;
        self.conn.flush()?;
        Ok(())
    }

    fn same_app_windows(&self, id: WinId) -> Vec<WinId> {
        let win = id as u32;
        let pid = self
            .prop32(win, self.a.wm_pid)
            .and_then(|v| v.first().copied());
        let class = self.class_of(win);
        // Client list and query-tree children are bottom-to-top; reverse so
        // the first entry is the top of the stack, like EnumWindows.
        let mut wins = self
            .prop32(self.root, self.a.client_list)
            .unwrap_or_else(|| {
                self.conn
                    .query_tree(self.root)
                    .ok()
                    .and_then(|r| r.reply().ok())
                    .map(|r| r.children)
                    .unwrap_or_default()
            });
        wins.reverse();
        let mut out: Vec<WinId> = wins
            .into_iter()
            .filter(|&w| {
                let same_app = match (pid, &class) {
                    (Some(pid), _) => {
                        self.prop32(w, self.a.wm_pid)
                            .and_then(|v| v.first().copied())
                            == Some(pid)
                    }
                    (None, Some(class)) => self.class_of(w).as_ref() == Some(class),
                    (None, None) => w == win,
                };
                same_app && self.is_zonable(w as u64)
            })
            .map(|w| w as u64)
            .collect();
        if out.is_empty() {
            out.push(id);
        }
        out
    }

    fn focus(&self, id: WinId) -> Result<()> {
        // Source indication 2 = pager: this is a user-requested switch, not
        // an application activating itself.
        self.send_client_message(id as u32, self.a.active_window, [2, CURRENT_TIME, 0, 0, 0])?;
        self.conn.flush()?;
        Ok(())
    }

    fn switch_desktop(&self, n: u8) -> Result<()> {
        // EWMH `_NET_CURRENT_DESKTOP`, 0-based. WMs without desktops ignore it.
        self.send_client_message(
            self.root,
            self.a.current_desktop,
            [u32::from(n.saturating_sub(1)), CURRENT_TIME, 0, 0, 0],
        )
    }

    fn move_to_desktop(&self, id: WinId, n: u8) -> Result<()> {
        // EWMH `_NET_WM_DESKTOP`: source indication 1 = application request.
        self.send_client_message(
            id as u32,
            self.a.wm_desktop,
            [u32::from(n.saturating_sub(1)), 1, 0, 0, 0],
        )
    }

    fn unpin_all(&self) -> Result<()> {
        let tree = self.conn.query_tree(self.root)?.reply()?;
        for win in tree.children {
            if self.is_always_on_top(win as u64) {
                if let Err(e) = self.set_always_on_top(win as u64, false) {
                    eprintln!("warn: unpin_all: window {win}: {e:#}");
                }
            }
        }
        Ok(())
    }

    fn find_by_pid(&self, pid: u32) -> Option<WinId> {
        let tree = self.conn.query_tree(self.root).ok()?.reply().ok()?;
        for child in tree.children {
            if let Some(v) = self.prop32(child, self.a.wm_pid) {
                if v.first() == Some(&pid) && self.is_zonable(child as u64) {
                    return Some(child as u64);
                }
            }
        }
        None
    }
}

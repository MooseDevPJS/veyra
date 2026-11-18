//! Several Veyra windows (one process each) working as one browser:
//! - the pinned tabs are the same in every window: `pins.json`, plus a
//!   `WM_PINS` post to the other windows whenever the set changes;
//! - a torn-off window held over another window's sidebar hides, and that
//!   sidebar shows the tab where it will land (`TAB_HOVER` messages);
//! - a window closed while others stay open hands its tabs to their
//!   "reopen closed tab" list. Every window saves the session when it
//!   closes, so the last one closed is what opens next time.

use std::{path::Path, time::Instant};

use serde::{Deserialize, Serialize};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, WPARAM},
    System::DataExchange::COPYDATASTRUCT,
    UI::WindowsAndMessaging::*,
};

use crate::{
    app::{App, TabId},
    ui::Drag,
};

/// WM_COPYDATA tags.
/// Veyra started again: open a window in the running program (JSON: URLs).
pub const OPEN_WINDOW: usize = 0x7AB5_0001;
pub const TAB_HOVER: usize = 0x7AB5_0002;
pub const TAB_HOVER_END: usize = 0x7AB5_0003;
pub const CLOSED_TABS: usize = 0x7AB5_0005;
/// Posted to the other windows: `pins.json` changed.
pub const WM_PINS: u32 = WM_APP + 9;
/// Sent by the test scripts: answers the tab counts (see main.rs).
pub const WM_TAB_COUNT: u32 = WM_APP + 10;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Pin {
    pub id: u64,
    pub url: String,
    pub title: String,
}

#[derive(Serialize, Deserialize)]
struct Moving {
    url: String,
    title: String,
    x: i32,
    y: i32,
}

pub fn load_pins(dir: &Path) -> Option<Vec<Pin>> {
    serde_json::from_str(&std::fs::read_to_string(dir.join("pins.json")).ok()?).ok()
}

fn save_pins(dir: &Path, pins: &[Pin]) {
    let path = dir.join("pins.json");
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(pins).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Unique across windows (time + process id), never 0.
fn new_pin_id() -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    (nanos ^ ((std::process::id() as u64) << 44)) | 1
}

/// The other Veyra windows (hidden ones too: a torn-off window over a sidebar).
pub fn other_windows(own: HWND) -> Vec<HWND> {
    crate::other_windows(own)
}

/// Veyra is already running (another process): ask it to open a window
/// with `urls`. True if it did (this process then just exits).
pub fn hand_to_running(urls: &[String]) -> bool {
    unsafe {
        let Ok(w) = FindWindowW(windows::core::w!("VeyraMain"), None) else { return false };
        if w.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }
        // The running Veyra may bring its new window to the front.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
        let json = serde_json::to_string(urls).unwrap_or_else(|_| "[]".into());
        send(w, HWND::default(), OPEN_WINDOW, &json, 5000) == Some(1)
    }
}

/// Sends `json` to another window; its answer, or None if it did not answer.
pub fn send(target: HWND, own: HWND, tag: usize, json: &str, timeout_ms: u32) -> Option<usize> {
    let data = COPYDATASTRUCT { dwData: tag, cbData: json.len() as u32, lpData: json.as_ptr() as *mut _ };
    let mut result = 0usize;
    let sent = unsafe {
        SendMessageTimeoutW(
            target,
            WM_COPYDATA,
            WPARAM(own.0 as usize),
            LPARAM(&data as *const _ as isize),
            SMTO_ABORTIFHUNG,
            timeout_ms,
            Some(&mut result),
        )
    };
    (sent.0 != 0).then_some(result)
}

impl App {
    // ---------- pinned tabs, shared by all windows ----------

    fn pins(&self) -> Vec<Pin> {
        self.tabs.iter().filter(|t| t.pinned).map(|t| Pin { id: t.pin, url: t.url.clone(), title: t.title.clone() }).collect()
    }

    /// At startup: the shared pinned tabs. False if there is no `pins.json`
    /// yet (then the session's pinned tabs are used, once).
    pub fn load_pins(&mut self) -> bool {
        let Some(pins) = load_pins(&self.data_dir) else { return false };
        for p in &pins {
            let id = self.add_tab(p.url.clone(), p.title.clone(), true);
            if let Some(t) = self.tab_mut(id) {
                t.pin = p.id;
                t.group = None;
            }
        }
        self.pins_sent = pins.iter().map(|p| p.id).collect();
        self.pins_loaded = true;
        true
    }

    /// After any change: when the set of pinned tabs (or their order) is not
    /// what the other windows have, save it and tell them.
    pub fn sync_pins(&mut self) {
        for t in &mut self.tabs {
            if t.pinned && t.pin == 0 {
                t.pin = new_pin_id();
            } else if !t.pinned {
                t.pin = 0;
            }
        }
        let ids: Vec<u64> = self.tabs.iter().filter(|t| t.pinned).map(|t| t.pin).collect();
        if ids == self.pins_sent {
            return;
        }
        self.pins_sent = ids;
        save_pins(&self.data_dir, &self.pins());
        for w in other_windows(self.hwnd) {
            unsafe {
                let _ = PostMessageW(Some(w), WM_PINS, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Keeps addresses and titles of the pinned tabs up to date on disk,
    /// unless another window changed the set meanwhile (its list wins).
    pub(crate) fn save_pin_details(&self) {
        let ours = self.pins();
        let on_disk: Vec<u64> = load_pins(&self.data_dir).unwrap_or_default().iter().map(|p| p.id).collect();
        if on_disk.is_empty() && ours.is_empty() {
            return;
        }
        if on_disk == ours.iter().map(|p| p.id).collect::<Vec<_>>() {
            save_pins(&self.data_dir, &ours);
        }
    }

    /// Another window changed the pinned tabs: same set and order here.
    /// Each window keeps its own page in them.
    pub fn reload_pins(&mut self) {
        let Some(pins) = load_pins(&self.data_dir) else { return };
        let ids: Vec<u64> = pins.iter().map(|p| p.id).collect();
        let gone: Vec<TabId> = self.tabs.iter().filter(|t| t.pinned && !ids.contains(&t.pin)).map(|t| t.id).collect();
        for id in gone {
            if self.active == Some(id) {
                // In use here: it stays, as a normal tab.
                if let Some(t) = self.tab_mut(id) {
                    t.pinned = false;
                    t.pin = 0;
                }
            } else {
                self.close(id);
            }
        }
        for p in &pins {
            if !self.tabs.iter().any(|t| t.pinned && t.pin == p.id) {
                let id = self.add_tab(p.url.clone(), p.title.clone(), true);
                if let Some(t) = self.tab_mut(id) {
                    t.pin = p.id;
                    t.group = None;
                }
            }
        }
        let (mut pinned, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.tabs).into_iter().partition(|t| t.pinned);
        pinned.sort_by_key(|t| ids.iter().position(|&i| i == t.pin));
        pinned.extend(rest);
        self.tabs = pinned;
        self.pins_sent = ids;
        self.session_dirty = true;
        self.redraw();
    }

    // ---------- a torn-off window over this window's sidebar ----------

    /// The torn-off window is over this window at `json`'s point: if that
    /// is on the sidebar, show its tab there (answer true: it hides).
    pub fn tab_hover(&mut self, json: &str) -> bool {
        let Ok(m) = serde_json::from_str::<Moving>(json) else { return false };
        let Some(pt) = self.drop_point(POINT { x: m.x, y: m.y }) else {
            self.end_incoming();
            return false;
        };
        let (x, y) = (pt.x as f32 / self.scale, pt.y as f32 / self.scale);
        let id = match self.incoming {
            Some(id) => id,
            None => {
                let id = self.add_tab(m.url, m.title, false);
                if let Some(t) = self.tab_mut(id) {
                    t.group = None;
                }
                self.incoming = Some(id);
                id
            }
        };
        self.incoming_at = Instant::now();
        self.ui.drag = Some(Drag { id, start_y: y, start_x: x, y, x, moving: true });
        self.redraw();
        true
    }

    /// The torn-off window left (or gave up): its preview row goes away.
    pub fn end_incoming(&mut self) {
        let Some(id) = self.incoming.take() else { return };
        if self.ui.drag.as_ref().is_some_and(|d| d.id == id) {
            self.ui.drag = None;
        }
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.tabs.remove(i);
        }
        self.redraw();
    }

    /// Safety net: a preview row whose window stopped talking (crashed).
    pub(crate) fn expire_incoming(&mut self) {
        if self.incoming.is_some() && self.incoming_at.elapsed().as_secs() >= 2 {
            self.end_incoming();
        }
    }

    // ---------- this window, torn off and following the pointer ----------

    /// Its only normal tab (what it carries): (url, title).
    fn carried(&self) -> Option<(TabId, String, String)> {
        self.tabs.iter().find(|t| !t.pinned).map(|t| (t.id, t.url.clone(), t.title.clone()))
    }

    /// Pointer moved while held: over another window's sidebar this window
    /// hides and that sidebar shows the tab; elsewhere it shows again.
    pub fn follow_hover(&mut self, pt: POINT) {
        if self.follow_last == Some((pt.x, pt.y)) {
            return;
        }
        self.follow_last = Some((pt.x, pt.y));
        let target = crate::shell::browser_window_under(pt, self.hwnd);
        if target != self.follow_target {
            if let Some(old) = self.follow_target {
                send(old, self.hwnd, TAB_HOVER_END, "", 100);
            }
            self.follow_target = target;
        }
        let accepted = match (target, self.carried()) {
            (Some(t), Some((_, url, title))) => {
                let json = serde_json::to_string(&Moving { url, title, x: pt.x, y: pt.y }).unwrap_or_default();
                send(t, self.hwnd, TAB_HOVER, &json, 100) == Some(1)
            }
            _ => false,
        };
        self.set_follow_hidden(accepted);
    }

    fn set_follow_hidden(&mut self, hidden: bool) {
        if hidden != self.follow_hidden {
            self.follow_hidden = hidden;
            unsafe {
                let _ = ShowWindow(self.hwnd, if hidden { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
        }
    }

    /// Released at `pt`: the tab goes into the Veyra window whose sidebar
    /// it was dropped on, if any; otherwise this window stays there.
    pub fn follow_dropped(&mut self, pt: POINT) {
        self.follow_last = None;
        let target = crate::shell::browser_window_under(pt, self.hwnd);
        if let (Some(t), Some((id, ..))) = (target, self.carried())
            && let Some(Some(at)) = crate::with_window(t, |a| a.drop_point(pt))
            && let Some(tab) = self.take_tab(id)
        {
            // The live tab goes over (no reload). If that window is busy
            // after all, it stays here.
            let mut slot = Some(tab);
            if crate::with_window(t, |a| a.adopt_dropped(slot.take().expect("tab"), at)).is_some() {
                self.follow_target = None;
                // Emptied (the pinned ones are in every window anyway): it closes.
                if self.tabs.iter().all(|t| t.pinned) {
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
                return;
            }
            if let Some(tab) = slot {
                let id = self.adopt_tab(tab);
                self.activate(id);
            }
        }
        if let Some(old) = self.follow_target.take() {
            send(old, self.hwnd, TAB_HOVER_END, "", 100);
        }
        self.set_follow_hidden(false);
        crate::fit_on_monitor(self.hwnd);
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    // ---------- closing one of several windows ----------

    /// This window closes while others stay: its tabs go to another
    /// window's "reopen closed tab" list.
    pub(crate) fn hand_over_closed(&self) {
        let Some(&w) = other_windows(self.hwnd).first() else { return };
        let tabs: Vec<(String, String)> = self
            .tabs
            .iter()
            .filter(|t| !t.pinned && Some(t.id) != self.incoming && t.url.starts_with("http"))
            .map(|t| (t.url.clone(), t.title.clone()))
            .collect();
        if !tabs.is_empty() {
            send(w, self.hwnd, CLOSED_TABS, &serde_json::to_string(&tabs).unwrap_or_default(), 1000);
        }
    }

    pub fn take_closed(&mut self, json: &str) {
        let Ok(tabs) = serde_json::from_str::<Vec<(String, String)>>(json) else { return };
        for (url, title) in tabs {
            self.push_closed(url, title);
        }
    }
}

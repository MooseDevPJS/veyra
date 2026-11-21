//! Window-level behavior of [`App`]: layout, rendering, mouse and keyboard
//! routing (sidebar / command bar / page), tab dragging, tooltips.

use std::time::Instant;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::core::Result;
use windows::{
    Win32::{
        Foundation::{POINT, RECT},
        Graphics::Direct2D::Common::D2D_RECT_F,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_LBUTTON, VK_RBUTTON, VK_SHIFT},
            WindowsAndMessaging::*,
        },
    },
    core::*,
};

use crate::{
    anim::Tween,
    app::{App, TabId, host_of, views},
    gfx::rect,
    settings::is_address,
    shortcuts::Action,
    store::{self, Suggestion},
    textfield::Key,
    ui::{Drag, Hit, PAGE_INSET, PAGE_RADIUS, Tip, key_chips},
};

pub const TIMER_ANIM: usize = 3;

/// Paces animation frames on the screen's refresh (see `start_frames`).
/// Dropping it stops the thread.
pub struct FrameClock {
    run: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// A frame was asked for and not drawn yet: no pile-up in the queue.
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl FrameClock {
    fn start(hwnd: windows::Win32::Foundation::HWND, every: u32) -> Self {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let run = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(AtomicBool::new(false));
        let (r, p, h) = (run.clone(), pending.clone(), hwnd.0 as isize);
        std::thread::spawn(move || {
            let mut n = 0u32;
            while r.load(Ordering::Acquire) {
                // Waits for the next refresh; without composition, ~60 Hz.
                if unsafe { windows::Win32::Graphics::Dwm::DwmFlush() }.is_err() {
                    std::thread::sleep(std::time::Duration::from_millis(16));
                }
                n += 1;
                if n % every != 0 || p.swap(true, Ordering::AcqRel) {
                    continue;
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_TIMER, WPARAM(TIMER_ANIM), LPARAM(0));
                }
            }
        });
        FrameClock { run, pending }
    }
}

impl Drop for FrameClock {
    fn drop(&mut self) {
        self.run.store(false, std::sync::atomic::Ordering::Release);
    }
}
pub const TIMER_TIP: usize = 4;
pub const TIMER_CARET: usize = 5;
pub const TIMER_NOTICE: usize = 8;
/// A torn-off window follows the pointer (see `follow_step`).
pub const TIMER_FOLLOW: usize = 10;

thread_local! {
    /// Torn-off window still held: where the pointer sits in it (window px).
    static FOLLOW: std::cell::Cell<Option<POINT>> = const { std::cell::Cell::new(None) };
}

/// The window moves with the pointer, held at `off` (window px), until the
/// mouse button is released. Works before the app exists (startup).
pub fn begin_follow(hwnd: windows::Win32::Foundation::HWND, off: POINT) {
    FOLLOW.set(Some(off));
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
        SetTimer(Some(hwnd), TIMER_FOLLOW, 8, None);
    }
    let _ = follow_step(hwnd);
}

pub enum Follow {
    Held(POINT),
    Dropped(POINT),
}

/// One step of `begin_follow`: where the pointer is, and whether the
/// button was released there.
pub fn follow_step(hwnd: windows::Win32::Foundation::HWND) -> Option<Follow> {
    let off = FOLLOW.get()?;
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetWindowPos(hwnd, None, pt.x - off.x, pt.y - off.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    let button = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
    if unsafe { GetAsyncKeyState(button.0 as i32) } as u16 & 0x8000 != 0 {
        return Some(Follow::Held(pt));
    }
    FOLLOW.set(None);
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_FOLLOW);
        windows::Win32::Media::timeEndPeriod(1);
    }
    Some(Follow::Dropped(pt))
}
/// Gap between the page card and the docked DevTools (DIPs).
const DOCK_GAP: f32 = 6.0;
/// Posted by `redraw()`; handled once per batch of changes.
pub const WM_RENDER: u32 = WM_APP + 1;

/// Mouse events the window forwards.
#[derive(Clone, Copy, PartialEq)]
pub enum Mouse {
    Move,
    LeftDown,
    LeftUp,
    LeftDouble,
    RightDown,
    RightUp,
    MiddleDown,
    MiddleUp,
    XDown,
    XUp,
    Wheel,
    HWheel,
    Leave,
}

impl App {
    // ---------- geometry ----------

    fn client(&self) -> RECT {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        r
    }

    fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    /// Page card in client pixels.
    pub fn page_rect(&self) -> RECT {
        let c = self.client();
        if self.is_fullscreen() {
            return c;
        }
        let inset = self.px(PAGE_INSET);
        let left = self.sidebar_px().max(inset);
        let right = match self.dock_rect() {
            Some(d) => d.left - self.px(DOCK_GAP),
            None => c.right - inset,
        };
        RECT { left, top: inset, right: right.max(left + 1), bottom: c.bottom - inset }
    }

    /// Client px the sidebar takes from the page (0 when hidden).
    fn sidebar_px(&self) -> i32 {
        self.px(self.ui.docked_width())
    }

    /// Docked DevTools pane (header included), when shown.
    pub fn dock_rect(&self) -> Option<RECT> {
        if !self.dock_visible() || self.is_fullscreen() {
            return None;
        }
        let c = self.client();
        let inset = self.px(PAGE_INSET);
        let left_limit = self.sidebar_px().max(inset) + self.px(240.0);
        let w = self.px(self.settings.devtools_width).min(c.right - inset - left_limit).max(self.px(200.0));
        Some(RECT { left: c.right - inset - w, top: inset, right: c.right - inset, bottom: c.bottom - inset })
    }

    /// Strip between page and DevTools that resizes the pane.
    fn on_dock_divider(&self, x: i32) -> bool {
        self.dock_rect().is_some_and(|d| x >= d.left - self.px(DOCK_GAP) - self.px(2.0) && x < d.left + self.px(3.0))
    }

    fn dock_header_dip(&self) -> Option<D2D_RECT_F> {
        let d = self.dock_rect()?;
        let s = self.scale;
        Some(rect(d.left as f32 / s, d.top as f32 / s, d.right as f32 / s, d.top as f32 / s + crate::devtools::HEADER))
    }

    /// Page bounds as the WebView sees them (origin at the card's corner).
    pub fn page_bounds(&self) -> RECT {
        let r = self.page_rect();
        RECT { left: 0, top: 0, right: (r.right - r.left).max(1), bottom: (r.bottom - r.top).max(1) }
    }

    pub(crate) fn page_dip(&self) -> D2D_RECT_F {
        let r = self.page_rect();
        let s = self.scale;
        rect(r.left as f32 / s, r.top as f32 / s, r.right as f32 / s, r.bottom as f32 / s)
    }

    fn height_dip(&self) -> f32 {
        self.client().bottom as f32 / self.scale
    }

    // ---------- layout & rendering ----------

    pub fn layout(&mut self) {
        let bounds = self.page_bounds();
        if let Some(v) = self.active_index().and_then(|i| self.tabs[i].view.as_ref()) {
            unsafe {
                let _ = v.controller.SetBounds(bounds);
            }
        }
        let radius = if self.is_fullscreen() { 0.0 } else { PAGE_RADIUS * self.scale };
        self.comp.set_page_rect(self.page_rect(), radius);
        self.place_peek();
        self.place_history();
        if let Some(d) = self.dock_rect() {
            self.place_dock(d);
        } else if self.dock.is_some() {
            self.place_dock(RECT::default()); // hides it (another tab is active)
        }
        self.render();
    }

    /// Redraws sidebar + overlay and keeps animating while anything moves.
    /// Schedules one repaint. Many changes in a row (title, URL, favicon
    /// events arriving together) cost a single render, done when the
    /// message queue gets to WM_RENDER.
    pub fn redraw(&mut self) {
        if !self.render_pending {
            self.render_pending = true;
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_RENDER, Default::default(), Default::default());
            }
        }
    }

    /// WM_RENDER: the deferred repaint, then animation frames if needed.
    pub fn flush_render(&mut self) {
        if !self.render_pending {
            return;
        }
        self.render_pending = false;
        self.sync_pins();
        self.render();
        if self.ui.animating() {
            self.start_frames();
        }
    }

    /// Caret blink while the command bar is open (2 repaints a second).
    pub fn blink(&mut self) {
        if self.ui.bar.open {
            self.redraw();
        } else {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_CARET);
            }
        }
    }

    /// One animation frame; stops the timer when everything settled.
    pub fn frame(&mut self) {
        if let Some(c) = &self.frame_clock {
            c.pending.store(false, std::sync::atomic::Ordering::Release);
        }
        // Closing command bar: finish the fade, then remove it.
        self.render();
        if !self.ui.animating() {
            self.stop_frames();
            self.settle_history();
        }
    }

    /// Animation frames, one per screen refresh (every other one in light PC
    /// mode). A timer made them uneven (16 or 31 ms apart, measured): a
    /// thread waits for each refresh (DwmFlush) and asks for a frame.
    fn start_frames(&mut self) {
        if self.frame_clock.is_none() {
            self.frame_clock = Some(FrameClock::start(self.hwnd, if self.settings.low_end_mode { 2 } else { 1 }));
        }
    }

    fn stop_frames(&mut self) {
        self.frame_clock = None;
    }

    /// Draws now, outside the usual WM_RENDER (before a motion starts).
    pub fn render_now(&mut self) {
        self.render();
    }

    fn render(&mut self) {
        let started = Instant::now();
        self.render_inner();
        crate::sys::perf("render", started, 8);
    }

    fn render_inner(&mut self) {
        self.card_landed();
        self.load_missing_icons();
        self.ui.bookmarks = self.store.borrow().bookmarks.iter().map(|b| (b.title.clone(), b.url.clone())).collect();
        self.ui.groups = self.groups.iter().map(|g| (g.id, g.name.clone(), g.collapsed)).collect();
        if let Some(id) = self.ui.reveal.take() {
            let tabs = views(&self.tabs, &self.icons, self.active);
            let height = self.height_dip();
            self.ui.scroll_to(&tabs, height, id);
        }
        let current = self.active_index().map(|i| self.tabs[i].url.clone()).unwrap_or_default();
        self.ui.has_page = current.starts_with("http") && !current.starts_with(&format!("https://{}/", crate::app::INTERNAL_HOST));
        self.ui.bookmarked = self.store.borrow().is_bookmarked(&current);
        if !self.exts_loaded && self.tabs.iter().any(|t| t.view.is_some()) {
            self.exts_loaded = true; // asked once; the answer fills `exts`
            self.refresh_extensions();
        }
        let c = self.client();
        let (w, h) = (c.right.max(1) as u32, c.bottom.max(1) as u32);
        let scale = self.scale;
        let hd = h as f32 / scale;
        let url = self.active_index().map(|i| host_of(&self.tabs[i].url).trim_start_matches("www.").to_string()).unwrap_or_default();
        let url = if url.is_empty() {
            String::new()
        } else if url == "browser.settings" {
            let i = self.active_index().unwrap_or(0);
            if self.tabs.get(i).is_some_and(|t| t.url.starts_with(crate::app::NEWTAB_URL)) {
                crate::i18n::t("Nuova scheda", "New tab").into()
            } else {
                crate::i18n::t("Impostazioni", "Settings").into()
            }
        } else {
            url
        };
        let page = self.page_dip();
        let page_radius = if self.is_fullscreen() { 0.0 } else { crate::ui::PAGE_RADIUS };
        let enter = crate::i18n::t("Invio", "Enter").to_string();
        // Frame under everything: solid when glass is off; with glass a tint
        // over the Acrylic keeps text readable on any wallpaper.
        let th = self.ui.theme;
        // Private: the night blue must read as such over any wallpaper.
        let alpha = if !th.glass { 1.0 } else if th.private { 0.9 } else if th.dark { 0.58 } else { 0.42 };
        let color = crate::gfx::rgba(th.frame(), alpha);
        if self.frame_drawn != (th.glass, w, h, th.dark) {
            self.frame_drawn = (th.glass, w, h, th.dark);
            let device = &self.comp.device;
            self.comp.frame.draw(device, w, h, scale, |dc| unsafe { dc.Clear(Some(&color)) });
        }
        {
            let tabs = views(&self.tabs, &self.icons, self.active);
            let ui = &mut self.ui;
            let device = &self.comp.device;
            // The sidebar surface only covers the sidebar (+ room for the
            // floating panel's shadow), not the whole window.
            let shown = (ui.visible || ui.compact || ui.peek.value() > 0.001) && self.fullscreen.is_none();
            let cw = if shown { ((ui.width + 24.0) * scale).ceil() as u32 } else { 1 };
            let icons = &self.icons;
            self.comp.chrome.draw(device, cw.min(w), h, scale, |dc| ui.draw_chrome(dc, &tabs, &url, icons, hd));
        }
        let bar_t = self.ui.bar.t.value();
        let header = self.dock_header_dip();
        let close_hover = self.dock_close_hover;
        let peek_card = self.peek_rect().map(|r| {
            let s = self.scale;
            rect(r.left as f32 / s, r.top as f32 / s, r.right as f32 / s, r.bottom as f32 / s)
        });
        let peek_buttons = self.peek_buttons();
        // The panel's slide follows the same frame as the blur and dimming.
        self.move_history();
        let history_t = self.history_progress().unwrap_or(0.0);
        let history_card = self.history_rect().filter(|_| history_t > 0.001 || self.history_open()).map(|r| {
            let s = self.scale;
            rect(r.left as f32 / s, r.top as f32 / s, r.right as f32 / s, r.bottom as f32 / s)
        });
        let peek_hover = self.peek.as_ref().and_then(|p| p.hover);
        let prompt = self.prompt_rect().zip(self.current_prompt()).map(|(card, p)| (card, p.host.clone(), p.what, p.hover));
        let notice = self.notice.clone();
        let card = self.card_box().and_then(|r| {
            let c = self.card.as_ref()?;
            let d = self.downloads.get(c.index)?;
            let buttons: Vec<_> = self.card_buttons(r).into_iter().map(|(b, w, l)| (b, w, l.to_string())).collect();
            Some((r, d.name.clone(), d.status(), d.progress(), d.flagged, buttons, c.hover))
        });
        let has_overlay = bar_t > 0.001
            || self.ui.tip.is_some()
            || self.ui.menu.is_some()
            || header.is_some()
            || peek_card.is_some()
            || history_card.is_some()
            || prompt.is_some()
            || notice.is_some()
            || card.is_some()
            || self.ui.find.is_some();
        {
            let ui = &mut self.ui;
            let icons = &self.icons;
            let device = &self.comp.device;
            if has_overlay {
                self.comp.overlay.draw(device, w, h, scale, |dc| {
                    if let Some(hd) = header {
                        ui.draw_dock_header(dc, hd, close_hover);
                    }
                    if let (Some(card), Some(buttons)) = (peek_card, peek_buttons) {
                        ui.draw_peek_chrome(dc, page, card, buttons, peek_hover);
                    }
                    if let Some(card) = history_card {
                        ui.dim_around(dc, page, page_radius, card, 18.0, history_t);
                    }
                    if let Some((card, host, what, hover)) = &prompt {
                        ui.draw_prompt(dc, *card, host, what, *hover);
                    }
                    if let Some((r, name, status, progress, flagged, buttons, hover)) = &card {
                        let buttons: Vec<_> = buttons.iter().map(|(b, w, l)| (*b, *w, l.as_str())).collect();
                        ui.draw_download_card(dc, *r, name, status, *progress, *flagged, &buttons, *hover, hd);
                    }
                    if let Some(text) = &notice {
                        ui.draw_notice(dc, page, text);
                    }
                    ui.draw_overlay(dc, page, icons, &enter)
                });
            } else {
                self.comp.overlay.draw(device, 1, 1, scale, |_| {});
            }
        }
        let blur = if self.peek.is_some() { 1.0 } else { bar_t.max(history_t) };
        self.comp.set_page_blur(14.0 * blur * self.scale);
        self.comp.commit();
    }

    /// Visual hosting: WebView2 must know when the window moves so its
    /// popups (select menus, autofill) open in the right place.
    pub fn parent_moved(&self) {
        for t in &self.tabs {
            if let Some(v) = &t.view {
                unsafe {
                    let _ = v.controller.NotifyParentWindowPositionChanged();
                }
            }
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale;
        self.layout();
    }

    /// Window title (taskbar, Alt+Tab): just the page title.
    pub fn refresh_title(&mut self) {
        let text = self
            .active_index()
            .map(|i| {
                let t = &self.tabs[i];
                if t.title.is_empty() { crate::ui::short_url(&t.url) } else { t.title.clone() }
            })
            .unwrap_or_else(|| "Veyra".into());
        // The taskbar tells private windows apart too.
        let text = if self.private { format!("{text} \u{2014} {}", crate::i18n::t("Privata", "Private")) } else { text };
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(text));
        }
        self.redraw();
    }

    pub(crate) fn favicon_path(&self, host: &str) -> std::path::PathBuf {
        let safe: String =
            host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        self.data_dir.join("favicons").join(format!("{safe}.png"))
    }

    /// Loads cached favicons from disk for hosts not seen yet this session.
    fn load_missing_icons(&mut self) {
        let hosts: Vec<String> = self
            .tabs
            .iter()
            .map(|t| host_of(&t.url))
            .filter(|h| !h.is_empty() && !self.icons.contains_key(h) && !self.icon_misses.contains(h))
            .collect();
        for host in hosts {
            // Veyra's own pages (new tab, settings): the app logo.
            const LOGO: &[u8] = include_bytes!("../assets/logo/veyra-64.png");
            let bytes = if host == crate::app::INTERNAL_HOST { Some(LOGO.to_vec()) } else { std::fs::read(self.favicon_path(&host)).ok() };
            match bytes.and_then(|b| self.ui.gfx.decode_icon(&b)) {
                Some(p) => {
                    self.icons.insert(host, p);
                    self.ui.icons_changed();
                }
                None => {
                    self.icon_misses.insert(host);
                }
            }
        }
    }

    // ---------- window frame ----------

    /// Non-client hit test for the frameless window (client coords, px).
    pub fn nc_hit(&self, x: i32, y: i32) -> u32 {
        if self.is_fullscreen() {
            return HTCLIENT; // the page is everything
        }
        let c = self.client();
        let maximized = unsafe { IsZoomed(self.hwnd).as_bool() };
        // While a menu, the command bar or a drag is active every point
        // belongs to it: never hand the click to Windows as a caption drag.
        if self.ui.menu.is_some() || self.ui.bar.open || self.ui.drag.is_some() || self.resizing {
            return HTCLIENT;
        }
        // DevTools header and the divider before it are ours too.
        if let Some(d) = self.dock_rect()
            && x >= d.left - self.px(DOCK_GAP) - self.px(2.0)
            && x < d.right
            && y >= d.top
            && y < d.top + self.px(crate::devtools::HEADER)
        {
            return HTCLIENT;
        }
        if self.on_dock_divider(x) && y > self.px(6.0) {
            return HTCLIENT;
        }
        // The sidebar's right edge is ours (drag to resize it).
        if self.on_resize_edge(x) && y > self.px(6.0) {
            return HTCLIENT;
        }
        // Hidden sidebar: a strip along the left edge reveals it. Only the
        // outermost pixels keep resizing the window.
        if !self.ui.visible && !self.ui.compact && x < self.px(10.0) && (maximized || x >= 2) && y > self.px(10.0) && y < c.bottom - self.px(10.0) {
            return HTCLIENT;
        }
        // Compact strip: ours below the window controls' height (hovering
        // it unfolds the sidebar); its top stays a window drag area.
        if self.ui.compact && !self.ui.floating() && x < self.sidebar_px() && (maximized || x >= 2) && y > self.px(crate::ui::PAD + 30.0) {
            return HTCLIENT;
        }
        if !maximized && !self.settings.title_bar {
            let b = self.px(6.0);
            let (l, t, r, bt) = (x < b, y < b, x >= c.right - b, y >= c.bottom - b);
            let edge = match (l, t, r, bt) {
                (true, true, _, _) => Some(HTTOPLEFT),
                (_, true, true, _) => Some(HTTOPRIGHT),
                (true, _, _, true) => Some(HTBOTTOMLEFT),
                (_, _, true, true) => Some(HTBOTTOMRIGHT),
                (true, ..) => Some(HTLEFT),
                (_, true, ..) => Some(HTTOP),
                (_, _, true, _) => Some(HTRIGHT),
                (.., true) => Some(HTBOTTOM),
                _ => None,
            };
            if let Some(e) = edge {
                return e;
            }
        }
        match self.sidebar_hit(x, y) {
            Some(Hit::Caption) if self.ui.drag.is_none() => HTCAPTION,
            // Lets Windows 11 show snap layouts over our maximize button.
            Some(Hit::Maximize) => HTMAXBUTTON,
            _ => {
                // The thin frame around the page card also drags the window.
                let p = self.page_rect();
                let in_page = x >= p.left && x < p.right && y >= p.top && y < p.bottom;
                let in_sidebar = x < self.sidebar_px();
                if !in_page && !in_sidebar && !self.ui.bar.open { HTCAPTION } else { HTCLIENT }
            }
        }
    }

    fn sidebar_hit(&self, x: i32, y: i32) -> Option<Hit> {
        let shown = self.ui.visible || self.ui.peek.value() > 0.5;
        if !shown {
            return None;
        }
        let xd = x as f32 / self.scale - self.ui.offset();
        let yd = y as f32 / self.scale;
        if xd < 0.0 || xd >= self.ui.width {
            return None;
        }
        let tabs = views(&self.tabs, &self.icons, self.active);
        self.ui.hit(&tabs, self.height_dip(), xd, yd)
    }

    fn in_page(&self, x: i32, y: i32) -> bool {
        let p = self.page_rect();
        x >= p.left && x < p.right && y >= p.top && y < p.bottom && self.sidebar_hit(x, y).is_none()
    }

    /// The pointer left the client area. That also happens when it moves
    /// onto a drag area of the sidebar (non-client for Windows), so the
    /// unfolded sidebar only closes when the pointer is really outside it:
    /// out of the window, or past its right edge.
    pub fn pointer_left(&mut self) {
        if self.ui.visible || self.ui.peek.target() <= 0.5 {
            return;
        }
        let mut pt = POINT::default();
        let mut wr = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let inside_window = pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom;
        let mut c = pt;
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut c);
        }
        let over_panel = (c.x as f32 / self.scale) < self.ui.width + 2.0;
        if !(inside_window && over_panel) {
            self.ui.peek.glide(0.0, if self.ui.anims { 220 } else { 0 });
            self.redraw();
        }
    }

    /// True when x (client px) is on the sidebar's draggable right edge.
    pub fn on_resize_edge(&self, x: i32) -> bool {
        let edge = self.px(self.ui.width);
        self.ui.visible && x >= edge - self.px(6.0) && x <= edge + self.px(6.0)
    }

    pub fn drag_resize(&mut self, x: i32) {
        let width = (x as f32 / self.scale).clamp(200.0, 420.0);
        if (width - self.ui.width).abs() >= 1.0 {
            self.ui.width = width;
            self.layout();
        }
    }

    pub fn end_resize(&mut self) {
        self.settings.sidebar_width = self.ui.width;
        self.settings.save(&self.data_dir);
    }

    /// Ctrl+S: docked <-> hidden (compact goes to docked).
    pub fn toggle_sidebar(&mut self) {
        let mode = if self.settings.sidebar == "docked" { "hidden" } else { "docked" };
        self.settings.sidebar = mode.into();
        self.ui.set_mode(mode);
        self.settings.save(&self.data_dir);
        self.layout();
    }

    /// Cursor for a client point, or None for the default arrow.
    pub fn cursor_at(&self, x: i32, y: i32) -> Option<HCURSOR> {
        if self.ui.bar.open {
            let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
            let r = self.ui.bar_rect(self.page_dip());
            if xd >= r.left + 40.0 && xd < r.right && yd >= r.top && yd < r.top + 56.0 {
                return unsafe { LoadCursorW(None, IDC_IBEAM).ok() };
            }
            return None;
        }
        if self.on_resize_edge(x) || self.on_dock_divider(x) {
            return unsafe { LoadCursorW(None, IDC_SIZEWE).ok() };
        }
        // Find bar: a text cursor on its field, the arrow on its buttons.
        if let Some(hit) = self.ui.find_hit(self.page_dip(), x as f32 / self.scale, y as f32 / self.scale) {
            return (hit == crate::ui::FindHit::Field).then(|| unsafe { LoadCursorW(None, IDC_IBEAM).ok() }).flatten();
        }
        // The page's cursor only while a live page is there (no tabs or an
        // unloaded tab: a stale "none" cursor would hide the pointer).
        let live = self.active_index().is_some_and(|i| self.tabs[i].view.is_some());
        if live && self.in_page(x, y) {
            return crate::app::PAGE_CURSOR.with(|c| c.borrow().get(&(self.hwnd.0 as isize)).copied());
        }
        None
    }

    // ---------- mouse ----------

    /// Routes a mouse event. `x`, `y` are client px; `keys`/`data` come from
    /// the message (MK_* flags, wheel delta / X button).
    pub fn mouse(&mut self, kind: Mouse, x: i32, y: i32, keys: u32, data: u32) {
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        if self.is_fullscreen() && self.ui.menu.is_none() {
            return self.forward(kind, x, y, keys, data);
        }

        // Sidebar resize drag in progress.
        if self.resizing {
            match kind {
                Mouse::Move => self.drag_resize(x),
                Mouse::LeftUp => {
                    self.resizing = false;
                    self.ui.edge_hover = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    self.end_resize();
                }
                _ => {}
            }
            return;
        }
        // Link preview open: it takes the mouse (outside clicks close it).
        if self.peek.is_some() && !self.resizing && !self.resizing_dock {
            self.peek_mouse(kind, x, y, keys, data);
            return;
        }
        // History panel: the same.
        if self.history_open() && !self.resizing && !self.resizing_dock {
            self.history_mouse(kind, x, y, keys, data);
            return;
        }
        // DevTools pane resize in progress.
        if self.resizing_dock {
            match kind {
                Mouse::Move => {
                    let right = self.client().right - self.px(PAGE_INSET);
                    let w = ((right - x) as f32 / self.scale).clamp(280.0, 1400.0);
                    if (w - self.settings.devtools_width).abs() >= 1.0 {
                        self.settings.devtools_width = w;
                        self.layout();
                    }
                }
                Mouse::LeftUp => {
                    self.resizing_dock = false;
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    self.settings.save(&self.data_dir);
                }
                _ => {}
            }
            return;
        }
        // DevTools header (close button) and divider.
        if let Some(hd) = self.dock_header_dip() {
            let over_close = crate::gfx::contains(&self.ui.dock_close_rect(hd), xd, yd);
            if over_close != self.dock_close_hover {
                self.dock_close_hover = over_close;
                self.redraw();
            }
            if kind == Mouse::LeftDown {
                if over_close {
                    self.close_devtools();
                    return;
                }
                if self.on_dock_divider(x) {
                    self.resizing_dock = true;
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                    return;
                }
            }
        }
        // Tab drag in progress.
        if self.ui.drag.is_some() {
            self.drag_mouse(kind, x, y);
            return;
        }
        // A menu stays only while it is being used: scrolling outside it
        // or reaching for the hidden/compact sidebar closes it (and the
        // gesture then does what it would do without the menu).
        if let Some(m) = &self.ui.menu {
            let c = self.client();
            let r = self.ui.menu_rect(m, c.right as f32 / self.scale, c.bottom as f32 / self.scale);
            let outside = !crate::gfx::contains(&r, xd, yd);
            let reveal = if self.ui.compact { self.sidebar_px() } else { self.px(10.0) };
            let to_sidebar = !self.ui.visible && kind == Mouse::Move && x < reveal && outside;
            let scroll_away = matches!(kind, Mouse::Wheel | Mouse::HWheel) && outside;
            if to_sidebar || scroll_away {
                self.close_menu();
            }
        }
        // Context menu: modal until a choice or a click outside.
        if self.ui.menu.is_some() {
            self.menu_mouse(kind, xd, yd);
            return;
        }
        // Command bar: modal over the page.
        if self.ui.bar.open {
            self.bar_mouse(kind, xd, yd);
            return;
        }
        // Permission and download cards over the page, and the find bar.
        if self.prompt_mouse(kind, x, y) || self.download_mouse(kind, x, y) || self.find_mouse(kind, x, y) {
            if self.mouse_in_page {
                self.forward(Mouse::Leave, x, y, 0, 0);
            }
            return;
        }
        // Hidden sidebar peeks in from the left edge; the compact one
        // unfolds when the pointer is on it.
        if !self.ui.visible && kind == Mouse::Move {
            let peeked = self.ui.peek.target() > 0.5;
            let reveal = if self.ui.compact { self.sidebar_px() } else { self.px(10.0) };
            if !peeked && x < reveal {
                self.ui.peek.glide(1.0, if self.ui.anims { 280 } else { 0 });
                self.redraw();
            } else if peeked && xd > self.ui.width + if self.ui.compact { 2.0 } else { 24.0 } {
                self.ui.peek.glide(0.0, if self.ui.anims { 220 } else { 0 });
                self.redraw();
            }
        }

        // The sidebar edge (both sides of it) resizes before anything else.
        let on_edge = !self.page_capture && self.on_resize_edge(x) && y > self.px(6.0);
        if on_edge != self.ui.edge_hover {
            self.ui.edge_hover = on_edge;
            self.redraw();
        }
        if on_edge && kind == Mouse::LeftDown {
            self.resizing = true;
            if self.mouse_in_page {
                self.forward(Mouse::Leave, x, y, 0, 0);
            }
            unsafe {
                SetCapture(self.hwnd);
            }
            return;
        }
        let hit = self.sidebar_hit(x, y);
        if !on_edge && (self.page_capture || (hit.is_none() && self.in_page(x, y))) {
            if self.ui.hover.is_some() {
                self.ui.set_hover(None);
                self.hide_tip();
                self.redraw();
            }
            self.forward(kind, x, y, keys, data);
            return;
        }
        if self.mouse_in_page {
            self.forward(Mouse::Leave, x, y, 0, 0);
        }

        match kind {
            Mouse::Move => {
                let hover = hit.filter(|h| *h != Hit::Caption);
                if hover != self.ui.hover {
                    self.ui.set_hover(hover);
                    self.hide_tip();
                    if hover.is_some_and(|h| tip_for(h).is_some()) {
                        unsafe {
                            SetTimer(Some(self.hwnd), TIMER_TIP, 450, None);
                        }
                    }
                    self.redraw();
                }
            }
            Mouse::Leave => {
                self.ui.set_hover(None);
                self.hide_tip();
                self.pointer_left();
                self.redraw();
            }
            Mouse::Wheel => {
                let tabs = views(&self.tabs, &self.icons, self.active);
                let max = self.ui.max_scroll(&tabs, self.height_dip());
                self.ui.scroll = (self.ui.scroll - (data as i32 as i16) as f32 / 120.0 * 108.0).clamp(0.0, max);
                self.redraw();
            }
            Mouse::LeftDown if self.on_resize_edge(x) => {
                self.resizing = true;
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Mouse::LeftDown => {
                self.hide_tip();
                if let Some(Hit::Tab(id) | Hit::Tile(id)) = hit {
                    self.ui.drag = Some(Drag { id, start_y: yd, start_x: xd, y: yd, x: xd, moving: false });
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                } else if let Some(h) = hit {
                    self.click(h);
                }
            }
            Mouse::MiddleUp => match hit {
                Some(Hit::Tab(id) | Hit::Tile(id)) => self.close(id),
                Some(Hit::Bookmark(i)) => {
                    if let Some((_, url)) = self.ui.bookmarks.get(i).cloned() {
                        self.add_tab(url, String::new(), false);
                        self.redraw();
                    }
                }
                _ => {}
            },
            Mouse::RightUp => match hit {
                Some(Hit::Tab(id) | Hit::Tile(id) | Hit::CloseTab(id)) => self.open_menu(id, xd, yd),
                Some(Hit::NewTab) => self.open_new_tab_menu(xd, yd),
                Some(Hit::Group(g)) => self.open_group_menu(g, xd, yd),
                Some(Hit::Bookmark(i)) => self.open_bookmark_menu(i, xd, yd),
                _ => {}
            },
            _ => {}
        }
    }

    fn click(&mut self, hit: Hit) {
        match hit {
            Hit::CloseTab(id) => self.close(id),
            Hit::Tab(id) | Hit::Tile(id) => self.activate(id),
            Hit::NewTab => self.run(Action::NewTab),
            Hit::Back => self.run(Action::Back),
            Hit::Forward => self.run(Action::Forward),
            Hit::Reload => self.run(Action::Reload),
            Hit::Url => self.open_bar(false),
            Hit::Star => self.toggle_bookmark(),
            Hit::Extensions => {
                let tabs = views(&self.tabs, &self.icons, self.active);
                if let Some((r, _)) = self.ui.layout(&tabs, self.height_dip()).into_iter().find(|(_, h)| *h == Hit::Extensions) {
                    self.open_extensions_menu(r.left - 8.0, r.bottom + 6.0);
                }
            }
            Hit::Group(g) => self.toggle_group(g),
            Hit::Bookmark(i) => {
                if let Some((_, url)) = self.ui.bookmarks.get(i).cloned() {
                    self.navigate_active(url);
                    self.focus_page();
                }
            }
            Hit::MoreBookmarks => self.open_more_bookmarks(),
            Hit::Settings => self.open_settings(),
            Hit::Downloads => {
                let tabs = views(&self.tabs, &self.icons, self.active);
                if let Some((r, _)) = self.ui.layout(&tabs, self.height_dip()).into_iter().find(|(_, h)| *h == Hit::Downloads) {
                    self.open_downloads_menu(r.left, r.top - 6.0);
                }
            }
            Hit::Minimize => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_SYSCOMMAND, windows::Win32::Foundation::WPARAM(SC_MINIMIZE as usize), windows::Win32::Foundation::LPARAM(0));
            },
            Hit::Maximize => self.toggle_maximize(),
            Hit::Close => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, Default::default(), Default::default());
            },
            Hit::Caption => {}
        }
        self.redraw();
    }

    pub fn toggle_maximize(&mut self) {
        unsafe {
            // Posted, not ShowWindow: the resize must not run while the app
            // is borrowed by this click.
            let max = IsZoomed(self.hwnd).as_bool();
            let cmd = if max { SC_RESTORE } else { SC_MAXIMIZE };
            let _ = PostMessageW(Some(self.hwnd), WM_SYSCOMMAND, windows::Win32::Foundation::WPARAM(cmd as usize), windows::Win32::Foundation::LPARAM(0));
        }
    }

    pub fn set_maximized(&mut self, maximized: bool) {
        self.ui.maximized = maximized;
    }

    /// Hover over our maximize button coming from WM_NCMOUSEMOVE.
    pub fn nc_hover(&mut self, over_max: bool) {
        let hover = over_max.then_some(Hit::Maximize);
        if hover != self.ui.hover {
            self.ui.set_hover(hover);
            self.redraw();
        }
    }

    /// Sends a mouse event to the active page (coordinates relative to the card).
    fn forward(&mut self, kind: Mouse, x: i32, y: i32, keys: u32, data: u32) {
        let Some(cc) = self.active_index().and_then(|i| self.tabs[i].view.as_ref()).map(|v| v.comp.clone()) else {
            return;
        };
        let p = self.page_rect();
        self.forward_to(&cc, p, kind, x, y, keys, data);
        if kind == Mouse::Move {
            self.probe_hidden_cursor(x - p.left, y - p.top);
        }
    }

    /// WebView2 workaround: once a page hid the pointer (`cursor: none`, a
    /// video's controls fading out), the runtime often does not report it
    /// coming back (no CursorChanged, or a NULL cursor), so it stayed hidden.
    /// While it is hidden, moving asks the page (at most every 150 ms) what
    /// cursor is under the pointer. `x`, `y`: px in the page.
    fn probe_hidden_cursor(&mut self, x: i32, y: i32) {
        if !crate::app::page_cursor_hidden(self.hwnd) || self.cursor_probe.is_some_and(|t| t.elapsed().as_millis() < 150) {
            return;
        }
        let Some(tab) = self.active_index().map(|i| &self.tabs[i]) else { return };
        let Some(webview) = tab.view.as_ref().map(|v| v.webview.clone()) else { return };
        let owner = crate::app::Owner(tab.id);
        self.cursor_probe = Some(Instant::now());
        let js = format!("(()=>{{const r=devicePixelRatio,e=document.elementFromPoint({x}/r,{y}/r);return e?getComputedStyle(e).cursor:''}})()");
        let done = webview2_com::ExecuteScriptCompletedHandler::create(Box::new(move |_, result: String| {
            let css: String = serde_json::from_str(&result).unwrap_or_default();
            if let Some(h) = crate::app::css_cursor(&css) {
                crate::app::page_cursor_unhidden(owner, h);
            }
            Ok(())
        }));
        unsafe {
            let _ = webview.ExecuteScript(&windows::core::HSTRING::from(js), &done);
        }
    }

    /// Sends a mouse event to a WebView shown at `p` (client px).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn forward_to(&mut self, cc: &ICoreWebView2CompositionController, p: RECT, kind: Mouse, x: i32, y: i32, keys: u32, data: u32) {
        let ev = match kind {
            Mouse::Move => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
            Mouse::LeftDown => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
            Mouse::LeftUp => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
            Mouse::LeftDouble => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
            Mouse::RightDown => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
            Mouse::RightUp => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
            Mouse::MiddleDown => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
            Mouse::MiddleUp => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
            Mouse::XDown => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN,
            Mouse::XUp => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
            Mouse::Wheel => COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
            Mouse::HWheel => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
            Mouse::Leave => COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
        };
        // Keep receiving the drag while a button is held (text selection,
        // scrollbars) even outside the card.
        match kind {
            Mouse::LeftDown | Mouse::RightDown | Mouse::MiddleDown => {
                self.page_capture = true;
                unsafe {
                    SetCapture(self.hwnd);
                }
            }
            Mouse::LeftUp | Mouse::RightUp | Mouse::MiddleUp => {
                self.page_capture = false;
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            _ => {}
        }
        self.mouse_in_page = kind != Mouse::Leave;
        // A link opened by this click goes to a background tab (app.rs, NewWindow).
        const MK_CONTROL: u32 = 0x8;
        if matches!(kind, Mouse::MiddleUp | Mouse::LeftUp) {
            let background = kind == Mouse::MiddleUp || keys & MK_CONTROL != 0;
            crate::app::BACKGROUND_CLICK.with(|c| c.set(background.then(Instant::now)));
        }
        let point = POINT { x: x - p.left, y: y - p.top };
        unsafe {
            let _ = cc.SendMouseInput(ev, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(keys as i32), data, point);
        }
    }

    // ---------- tab drag ----------

    fn drag_mouse(&mut self, kind: Mouse, x: i32, y: i32) {
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        let c = self.client();
        // Leaving the sidebar (onto the page or out of the window) pulls the
        // tab out: a preview follows the pointer.
        let side_right = self.ui.width + self.ui.offset() + 40.0;
        let outside = xd > side_right || x < -self.px(40.0) || y < -self.px(40.0) || y > c.bottom + self.px(40.0);
        let mut screen = POINT { x, y };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut screen);
        }
        match kind {
            Mouse::Move => {
                let Some(id) = self.ui.drag.as_ref().map(|d| d.id) else { return };
                // A tab another window is bringing here (multi.rs): it only
                // follows the pointer up and down the list.
                let incoming = self.incoming == Some(id);
                let pinned = self.tabs.iter().any(|t| t.id == id && t.pinned);
                let alone = self.tabs.iter().filter(|t| !t.pinned).count() == 1;
                if let Some(d) = &mut self.ui.drag {
                    let far = (xd - d.start_x).abs() > 4.0 || (yd - d.start_y).abs() > 4.0;
                    d.x = xd;
                    d.y = yd;
                    // Tiles move in two directions; list rows only vertically
                    // (a sideways wobble is still a click).
                    if (yd - d.start_y).abs() > 4.0 || outside || ((pinned || alone) && far) {
                        d.moving = true;
                    }
                }
                let moving = self.ui.drag.as_ref().is_some_and(|d| d.moving);
                // Out of the sidebar the tab becomes its own window right away,
                // which then follows the pointer; a window's only tab moves
                // the window itself as soon as it moves. Pinned tabs stay in
                // their grid.
                if !incoming && !pinned && ((alone && moving) || outside) && let Some(d) = self.ui.drag.take() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    self.tear_off(d, screen);
                }
                self.redraw();
            }
            Mouse::LeftUp => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                // Brought by another window: it lands when that window hands it over.
                if self.ui.drag.as_ref().is_some_and(|d| Some(d.id) == self.incoming) {
                    return;
                }
                let Some(d) = self.ui.drag.take() else { return };
                let pinned = self.tabs.iter().any(|t| t.id == d.id && t.pinned);
                if !d.moving {
                    self.activate(d.id);
                } else if pinned {
                    self.drop_tile(d.id, d.x, d.y);
                } else {
                    self.drop_tab(d.id, d.y);
                }
                self.redraw();
            }
            _ => {}
        }
    }

    /// Opens the dragged tab in a new window the size of this one (its normal
    /// size if this one is maximized), still held by the pointer: the new
    /// window follows it until the button is released.
    fn tear_off(&mut self, d: Drag, screen: POINT) {
        let Some(t) = self.tabs.iter().find(|t| t.id == d.id) else { return };
        if self.tabs.iter().filter(|t| !t.pinned).count() == 1 {
            // Its only tab: this window itself follows the pointer (no
            // empty window left behind).
            let mut wr = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut wr);
                if IsZoomed(self.hwnd).as_bool() {
                    let _ = ShowWindow(self.hwnd, SW_RESTORE);
                    let _ = GetWindowRect(self.hwnd, &mut wr);
                    let w = wr.right - wr.left;
                    // Restored under the pointer, at the same spot across its width.
                    wr.left = screen.x - w / 2;
                    wr.top = screen.y - self.px(d.start_y);
                }
            }
            begin_follow(self.hwnd, POINT { x: screen.x - wr.left, y: screen.y - wr.top });
            return;
        }
        let url = t.url.clone();
        // Where it was grabbed, relative to its row in the list.
        let grab = {
            let tabs = views(&self.tabs, &self.icons, self.active);
            let rows = self.ui.layout(&tabs, self.height_dip());
            rows.iter()
                .find(|(_, h)| matches!(h, Hit::Tab(i) | Hit::Tile(i) if *i == d.id))
                .map(|(r, _)| ((d.start_x - r.left).clamp(0.0, r.right - r.left), (d.start_y - r.top).clamp(0.0, r.bottom - r.top)))
                .unwrap_or((24.0, 16.0))
        };
        // Its row in the new window, which lists the pinned tabs and only
        // this tab (same settings, so the same sidebar).
        let row = {
            let mut tabs = views(&self.tabs, &self.icons, self.active);
            tabs.retain(|t| t.id == d.id || t.pinned);
            for t in tabs.iter_mut().filter(|t| t.id == d.id) {
                (t.active, t.group) = (true, None);
            }
            for t in tabs.iter_mut().filter(|t| t.id != d.id) {
                t.active = false;
            }
            let scroll = std::mem::replace(&mut self.ui.scroll, 0.0);
            let rows = self.ui.layout(&tabs, self.height_dip());
            self.ui.scroll = scroll;
            rows.iter().find(|(_, h)| *h == Hit::Tab(d.id)).map(|(r, _)| *r)
        };
        let (w, h) = unsafe {
            let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let mut wr = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut wr);
            let placed = self.fullscreen.or_else(|| GetWindowPlacement(self.hwnd, &mut wp).ok().map(|_| wp));
            match placed {
                Some(p) if p.showCmd == SW_SHOWMAXIMIZED.0 as u32 || self.fullscreen.is_some() => {
                    let r = p.rcNormalPosition;
                    (r.right - r.left, r.bottom - r.top)
                }
                _ => (wr.right - wr.left, wr.bottom - wr.top),
            }
        };
        let (mut gx, gy) = grab;
        let r = row.unwrap_or(D2D_RECT_F { left: 8.0, top: 100.0, right: 200.0, bottom: 136.0 });
        if self.ui.compact {
            // Only the icon column shows: held by the icon.
            gx = (crate::ui::COMPACT_W / 2.0 - r.left).max(0.0);
        }
        // Pointer in the new window (px from its top-left corner).
        let mut origin = POINT::default();
        let mut wr = RECT::default();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut origin);
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let off = POINT { x: origin.x - wr.left + self.px(r.left + gx), y: origin.y - wr.top + self.px(r.top + gy) };
        let _ = url;
        // The page moves alive (no reload: a video keeps playing).
        let Some(tab) = self.take_tab(d.id) else { return };
        let opts = crate::WindowOpts {
            at: Some((screen.x - off.x, screen.y - off.y)),
            size: Some((w, h)),
            content: crate::Content::Tab(tab),
            follow: Some(off),
            private: self.private,
        };
        if let Err(back) = crate::open_window(opts) {
            self.take_back(back);
        }
    }

    /// A window could not be opened: its tab comes back here.
    fn take_back(&mut self, back: crate::WindowOpts) {
        if let crate::Content::Tab(tab) = back.content {
            let id = self.adopt_tab(tab);
            self.activate(id);
            self.redraw();
        }
    }

    // ---------- moving tabs between windows ----------

    /// `screen` in client px if it is on the sidebar (where it shows, or
    /// where the hidden one appears): the only place a tab from another
    /// window is taken.
    pub(crate) fn drop_point(&self, screen: POINT) -> Option<POINT> {
        let mut pt = screen;
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut pt);
        }
        let c = self.client();
        let zone = if self.ui.compact { self.sidebar_px() } else { self.px(self.ui.width) };
        (pt.x >= 0 && pt.x < zone && pt.y >= 0 && pt.y < c.bottom).then_some(pt)
    }

    /// A live tab dropped on this window's sidebar at `pt` (client px):
    /// it takes the place of its preview row (multi.rs, `tab_hover`).
    pub(crate) fn adopt_dropped(&mut self, tab: crate::app::Tab, pt: POINT) {
        self.end_incoming();
        let id = self.adopt_tab(tab);
        // Dropped over the tab list: put it exactly there.
        if self.ui.visible && (pt.x as f32) < self.ui.width * self.scale {
            self.drop_tab(id, pt.y as f32 / self.scale);
        }
        self.activate(id);
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.redraw();
    }

    /// Moves a tab to the slot under `y` in the unpinned list.
    fn drop_tab(&mut self, id: TabId, y: f32) {
        let to = {
            let tabs = views(&self.tabs, &self.icons, self.active);
            self.ui.drop_index(&tabs, y)
        };
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        if self.tabs[from].pinned {
            return;
        }
        let tab = self.tabs.remove(from);
        // `to` counts unpinned tabs; map it back to an index in `tabs`.
        let mut seen = 0;
        let mut at = self.tabs.len();
        for (i, t) in self.tabs.iter().enumerate() {
            if !t.pinned {
                if seen == to {
                    at = i;
                    break;
                }
                seen += 1;
            }
        }
        self.tabs.insert(at, tab);
        self.session_dirty = true;
    }

    /// Moves a pinned tab to the tile slot under (x, y) in the grid.
    fn drop_tile(&mut self, id: TabId, x: f32, y: f32) {
        let to = {
            let tabs = views(&self.tabs, &self.icons, self.active);
            self.ui.tile_slot(&tabs, x, y)
        };
        let Some(from) = self.tabs.iter().position(|t| t.id == id) else { return };
        let tab = self.tabs.remove(from);
        // `to` counts pinned tabs; map it back to an index in `tabs`.
        let mut seen = 0;
        let mut at = self.tabs.iter().rposition(|t| t.pinned).map_or(0, |i| i + 1);
        for (i, t) in self.tabs.iter().enumerate() {
            if t.pinned {
                if seen == to {
                    at = i;
                    break;
                }
                seen += 1;
            }
        }
        self.tabs.insert(at, tab);
        self.session_dirty = true;
    }

    /// Opens the tab in a new window (new process sharing the profile) and
    /// removes it here.
    fn detach(&mut self, id: TabId, at: Option<POINT>) {
        let mut pt = POINT::default();
        match at {
            Some(p) => pt = p,
            None => unsafe {
                let _ = GetCursorPos(&mut pt);
            },
        }
        let Some(tab) = self.take_tab(id) else { return };
        let opts = crate::WindowOpts {
            at: Some((pt.x - self.px(120.0), pt.y - self.px(20.0))),
            size: None,
            content: crate::Content::Tab(tab),
            follow: None,
            private: self.private,
        };
        if let Err(back) = crate::open_window(opts) {
            self.take_back(back);
        }
    }

    // ---------- tab context menu ----------

    fn open_menu(&mut self, id: TabId, x: f32, y: f32) {
        use crate::ui::{Menu, MenuCmd, MenuItem};
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return };
        let pinned = self.tabs[i].pinned;
        let key = |a: Action| self.settings.shortcuts.0.get(&a).and_then(|k| k.first().cloned()).unwrap_or_default();
        let item = |label: &str, cmd, keys: String, sep| MenuItem { label: label.into(), cmd, keys, sep };
        let mut items = vec![
            item(if pinned { crate::i18n::t("Sblocca dalla griglia", "Unpin") } else { crate::i18n::t("Fissa in alto", "Pin to top") }, MenuCmd::Pin, key(Action::PinTab), false),
            item(crate::i18n::t("Duplica", "Duplicate"), MenuCmd::Duplicate, String::new(), false),
            item(crate::i18n::t("Ricarica", "Reload"), MenuCmd::Reload, key(Action::Reload), false),
            item(crate::i18n::t("Copia indirizzo", "Copy address"), MenuCmd::CopyUrl, key(Action::CopyUrl), false),
            item(crate::i18n::t("Sposta in una nuova finestra", "Move to new window"), MenuCmd::NewWindow, String::new(), true),
            item(crate::i18n::t("Chiudi", "Close"), MenuCmd::Close, key(Action::CloseTab), true),
        ];
        if pinned {
            // Pinned tabs are in every window already.
            items.retain(|i| !matches!(i.cmd, MenuCmd::NewWindow));
        }
        if self.private {
            // Pinned tabs are shared with every window: none here.
            items.retain(|i| !matches!(i.cmd, MenuCmd::Pin));
        }
        if self.tabs.len() > 1 {
            items.push(item(crate::i18n::t("Chiudi le altre schede", "Close other tabs"), MenuCmd::CloseOthers, String::new(), false));
        }
        // Groups: their own block, before the closing actions.
        let current = self.tabs[i].group;
        let mut group_items = Vec::new();
        if current.is_none() {
            group_items.push(item(crate::i18n::t("Nuovo gruppo con questa scheda", "New group with this tab"), MenuCmd::NewGroup, String::new(), false));
        }
        for g in self.groups.iter().filter(|g| Some(g.id) != current).take(5) {
            let label = format!("{} \u{201C}{}\u{201D}", crate::i18n::t("Sposta in", "Move to"), g.name);
            group_items.push(MenuItem { label, cmd: MenuCmd::MoveToGroup(g.id), keys: String::new(), sep: false });
        }
        if current.is_some() {
            group_items.push(item(crate::i18n::t("Togli dal gruppo", "Remove from group"), MenuCmd::Ungroup, String::new(), false));
        }
        if let Some(first) = group_items.first_mut() {
            first.sep = true;
        }
        let close_at = items.iter().position(|it| it.cmd == MenuCmd::Close).unwrap_or(items.len());
        items.splice(close_at..close_at, group_items);
        if self.tabs[i + 1..].iter().any(|t| !t.pinned) {
            items.push(item(crate::i18n::t("Chiudi le schede sotto", "Close tabs below"), MenuCmd::CloseBelow, String::new(), false));
        }
        let mut t = Tween::new(0.0);
        t.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        self.ui.menu = Some(Menu { tab: id, x, y, items, hover: None, t, page: None });
        self.redraw();
    }

    fn open_group_menu(&mut self, g: u32, x: f32, y: f32) {
        use crate::i18n::t;
        use crate::ui::{Menu, MenuCmd, MenuItem};
        let item = |label: &str, cmd, sep| MenuItem { label: label.into(), cmd, keys: String::new(), sep };
        let items = vec![
            item(t("Rinomina", "Rename"), MenuCmd::RenameGroup(g), false),
            item(t("Separa il gruppo", "Ungroup"), MenuCmd::DissolveGroup(g), false),
            item(t("Chiudi le schede del gruppo", "Close the group's tabs"), MenuCmd::CloseGroup(g), true),
        ];
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        let tab = self.active.unwrap_or(0);
        self.ui.menu = Some(Menu { tab, x, y, items, hover: None, t: tw, page: None });
        self.redraw();
    }

    /// The bookmarks that do not fit in the two rows, as a menu under the
    /// "more" button.
    fn open_more_bookmarks(&mut self) {
        use crate::ui::{Menu, MenuCmd, MenuItem};
        let tabs = views(&self.tabs, &self.icons, self.active);
        let height = self.height_dip();
        let Some((r, _)) = self.ui.layout(&tabs, height).into_iter().find(|(_, h)| *h == Hit::MoreBookmarks) else { return };
        let first = self.ui.layout(&tabs, height).iter().filter(|(_, h)| matches!(h, Hit::Bookmark(_))).count();
        let fit = ((height - r.bottom - 24.0) / 32.0).max(1.0) as usize;
        let items: Vec<MenuItem> = self.ui.bookmarks[first..]
            .iter()
            .take(fit)
            .map(|(title, url)| MenuItem {
                label: if title.is_empty() { crate::ui::short_url(url) } else { title.clone() },
                cmd: MenuCmd::OpenHere(url.clone()),
                keys: String::new(),
                sep: false,
            })
            .collect();
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        let tab = self.active.unwrap_or(0);
        self.ui.menu = Some(Menu { tab, x: r.left, y: r.bottom + 4.0, items, hover: None, t: tw, page: None });
        self.redraw();
    }

    /// Opens a plain menu at (x, y) DIPs (bookmarks, extensions...).
    pub(crate) fn show_menu(&mut self, items: Vec<crate::ui::MenuItem>, x: f32, y: f32) {
        self.downloads_menu = false;
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        self.ui.menu_anchor = (x, y);
        let tab = self.active.unwrap_or(0);
        self.ui.menu = Some(crate::ui::Menu { tab, x, y, items, hover: None, t: tw, page: None });
        self.redraw();
    }

    fn run_extensions_store(&mut self) {
        let id = self.add_tab("https://chromewebstore.google.com/category/extensions".into(), String::new(), false);
        self.activate(id);
    }

    /// Settings page, extensions section open.
    fn manage_extensions(&mut self) {
        let url = format!("{}#extensions", crate::app::SETTINGS_URL);
        match self.tabs.iter().find(|t| t.url.starts_with(crate::app::SETTINGS_URL)).map(|t| t.id) {
            Some(id) => {
                self.activate(id);
                self.navigate_active(url);
            }
            None => self.open(url),
        }
    }

    /// Bookmark menu: removing is a deliberate choice, never a single click.
    /// Right-click on "New tab": a new tab, or a new private window.
    fn open_new_tab_menu(&mut self, x: f32, y: f32) {
        use crate::i18n::t;
        use crate::ui::{Menu, MenuCmd, MenuItem};
        let key = |a: Action| self.settings.shortcuts.0.get(&a).and_then(|k| k.first().cloned()).unwrap_or_default();
        let items = vec![
            MenuItem { label: t("Nuova scheda", "New tab").into(), cmd: MenuCmd::Shortcut(Action::NewTab), keys: key(Action::NewTab), sep: false },
            MenuItem {
                label: t("Nuova finestra privata", "New private window").into(),
                cmd: MenuCmd::Shortcut(Action::PrivateWindow),
                keys: key(Action::PrivateWindow),
                sep: false,
            },
        ];
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        let tab = self.active.unwrap_or(0);
        self.ui.menu = Some(Menu { tab, x, y, items, hover: None, t: tw, page: None });
        self.redraw();
    }

    fn open_bookmark_menu(&mut self, i: usize, x: f32, y: f32) {
        use crate::i18n::t;
        use crate::ui::{Menu, MenuCmd, MenuItem};
        let Some((_, url)) = self.ui.bookmarks.get(i).cloned() else { return };
        let item = |label: &str, cmd, sep| MenuItem { label: label.into(), cmd, keys: String::new(), sep };
        let items = vec![
            item(t("Apri", "Open"), MenuCmd::OpenHere(url.clone()), false),
            item(t("Apri in una nuova scheda", "Open in new tab"), MenuCmd::OpenTab(url.clone()), false),
            item(t("Apri in una nuova finestra", "Open in new window"), MenuCmd::OpenWindow(url.clone()), false),
            item(t("Copia indirizzo", "Copy address"), MenuCmd::CopyText(url.clone()), false),
            item(t("Rimuovi dai preferiti", "Remove bookmark"), MenuCmd::RemoveBookmark(url), true),
        ];
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.set_hover(None);
        let tab = self.active.unwrap_or(0);
        self.ui.menu = Some(Menu { tab, x, y, items, hover: None, t: tw, page: None });
        self.redraw();
    }

    /// Group rename uses the floating bar as a one-line prompt.
    fn open_rename(&mut self, g: u32) {
        let name = self.groups.iter().find(|x| x.id == g).map(|x| x.name.clone()).unwrap_or_default();
        self.open_bar(false);
        self.ui.bar.rename = Some(g);
        self.ui.bar.field.set(&name, true);
        self.ui.bar.items.clear();
        self.redraw();
    }

    pub fn close_menu(&mut self) {
        self.downloads_menu = false;
        if let Some(m) = self.ui.menu.take() {
            // A page menu must always be answered, even with no choice.
            if let Some(p) = m.page {
                unsafe {
                    let _ = p.deferral.Complete();
                }
            }
            self.redraw();
        }
    }

    /// Right-click in a page: our menu with the essentials for what is under
    /// the pointer (link, image, selection, text field, page).
    pub fn open_page_menu(&mut self, id: TabId, args: ICoreWebView2ContextMenuRequestedEventArgs, deferral: ICoreWebView2Deferral) {
        use crate::i18n::t;
        use crate::ui::{Menu, MenuCmd, MenuItem, PageMenu};
        self.close_menu();
        let pstr = |f: &dyn Fn(*mut PWSTR) -> Result<()>| -> String {
            let mut p = PWSTR::null();
            if f(&mut p).is_ok() && !p.is_null() {
                webview2_com::CoTaskMemPWSTR::from(p).to_string()
            } else {
                String::new()
            }
        };
        let flag = |f: &dyn Fn(*mut BOOL) -> Result<()>| -> bool {
            let mut b = BOOL(0);
            f(&mut b).is_ok() && b.as_bool()
        };
        let (Ok(target), Ok(menu_items)) = (unsafe { args.ContextMenuTarget() }, unsafe { args.MenuItems() }) else {
            unsafe {
                let _ = deferral.Complete();
            }
            return;
        };
        // Built-in commands by name ("copy", "saveImageAs", ...).
        let mut builtin = std::collections::HashMap::new();
        let mut n = 0u32;
        unsafe {
            let _ = menu_items.Count(&mut n);
            for i in 0..n {
                if let Ok(item) = menu_items.GetValueAtIndex(i) {
                    let name = pstr(&|p| item.Name(p));
                    let mut cmd = 0;
                    let _ = item.CommandId(&mut cmd);
                    builtin.insert(name, cmd);
                }
            }
        }
        let link = if flag(&|b| unsafe { target.HasLinkUri(b) }) { pstr(&|p| unsafe { target.LinkUri(p) }) } else { String::new() };
        let image = if flag(&|b| unsafe { target.HasSourceUri(b) }) { pstr(&|p| unsafe { target.SourceUri(p) }) } else { String::new() };
        let selection = if flag(&|b| unsafe { target.HasSelection(b) }) { pstr(&|p| unsafe { target.SelectionText(p) }) } else { String::new() };
        let editable = flag(&|b| unsafe { target.IsEditable(b) });

        let mut items: Vec<MenuItem> = Vec::new();
        let sep = std::cell::Cell::new(false);
        let push = |items: &mut Vec<MenuItem>, label: &str, cmd: MenuCmd, keys: &str| {
            items.push(MenuItem { label: label.into(), cmd, keys: keys.into(), sep: sep.replace(false) });
        };
        let b = |name: &str| builtin.get(name).copied();
        if !link.is_empty() {
            push(&mut items, t("Apri in una nuova scheda", "Open in new tab"), MenuCmd::OpenTab(link.clone()), "");
            push(&mut items, t("Apri in una nuova finestra", "Open in new window"), MenuCmd::OpenWindow(link.clone()), "");
            push(&mut items, t("Copia link", "Copy link"), MenuCmd::CopyText(link.clone()), "");
            sep.set(true);
        }
        // An image, also inside a link (the engine then only offers its
        // image commands; the address comes from the page script).
        if image.starts_with("http") || b("copyImage").is_some() {
            if image.starts_with("http") {
                self.context_image = image.clone();
            }
            push(&mut items, t("Apri immagine in una nuova scheda", "Open image in new tab"), MenuCmd::OpenImage, "");
            push(&mut items, t("Scarica immagine", "Download image"), MenuCmd::DownloadImage, "");
            if let Some(c) = b("copyImage") {
                push(&mut items, t("Copia immagine", "Copy image"), MenuCmd::Builtin(c), "");
            }
            match b("copyImageLocation") {
                Some(c) => push(&mut items, t("Copia indirizzo immagine", "Copy image address"), MenuCmd::Builtin(c), ""),
                None => push(&mut items, t("Copia indirizzo immagine", "Copy image address"), MenuCmd::CopyText(image.clone()), ""),
            }
            sep.set(true);
        }
        if editable {
            for (name, it, en, keys) in [
                ("undo", "Annulla", "Undo", "Ctrl+Z"),
                ("cut", "Taglia", "Cut", "Ctrl+X"),
                ("copy", "Copia", "Copy", "Ctrl+C"),
                ("paste", "Incolla", "Paste", "Ctrl+V"),
                ("selectAll", "Seleziona tutto", "Select all", "Ctrl+A"),
            ] {
                if let Some(c) = b(name) {
                    push(&mut items, t(it, en), MenuCmd::Builtin(c), keys);
                }
            }
            sep.set(true);
        } else if !selection.is_empty() {
            if let Some(c) = b("copy") {
                push(&mut items, t("Copia", "Copy"), MenuCmd::Builtin(c), "Ctrl+C");
            }
            let short: String = selection.chars().take(24).collect();
            let more = if selection.chars().count() > 24 { "…" } else { "" };
            let label = format!("{} \u{201C}{short}{more}\u{201D}", t("Cerca", "Search for"));
            items.push(MenuItem { label, cmd: MenuCmd::SearchText(selection.clone()), keys: String::new(), sep: sep.replace(false) });
            sep.set(true);
        }
        if link.is_empty() && image.is_empty() && selection.is_empty() && !editable {
            push(&mut items, t("Indietro", "Back"), MenuCmd::Back, "Alt+\u{2190}");
            push(&mut items, t("Avanti", "Forward"), MenuCmd::Forward, "Alt+\u{2192}");
            push(&mut items, t("Ricarica", "Reload"), MenuCmd::PageReload, "Ctrl+R");
            sep.set(true);
            if let Some(c) = b("saveAs") {
                push(&mut items, t("Salva pagina con nome…", "Save page as…"), MenuCmd::Builtin(c), "Ctrl+S");
            }
            if let Some(c) = b("print") {
                push(&mut items, t("Stampa…", "Print…"), MenuCmd::Builtin(c), "Ctrl+P");
            }
            sep.set(true);
        }
        if let Some(c) = b("inspectElement") {
            push(&mut items, t("Ispeziona", "Inspect"), MenuCmd::Inspect(c), "");
        }
        if items.is_empty() {
            unsafe {
                let _ = deferral.Complete();
            }
            return;
        }
        let mut loc = POINT::default();
        unsafe {
            let _ = args.Location(&mut loc);
        }
        let p = self.page_rect();
        let (x, y) = ((p.left + loc.x) as f32 / self.scale, (p.top + loc.y) as f32 / self.scale);
        let mut tw = Tween::new(0.0);
        tw.go(1.0, if self.ui.anims { 110 } else { 50 });
        self.hide_tip();
        self.ui.menu = Some(Menu { tab: id, x, y, items, hover: None, t: tw, page: Some(PageMenu { args, deferral }) });
        self.redraw();
    }

    fn menu_mouse(&mut self, kind: Mouse, x: f32, y: f32) {
        let (w, h) = (self.client().right as f32 / self.scale, self.height_dip());
        match kind {
            Mouse::Move => {
                let hover = self.ui.menu_item_at(w, h, x, y);
                if let Some(m) = &mut self.ui.menu
                    && m.hover != hover
                {
                    m.hover = hover;
                    self.redraw();
                }
            }
            Mouse::LeftDown | Mouse::RightDown | Mouse::MiddleDown => {
                if !self.ui.in_menu(w, h, x, y) {
                    self.close_menu();
                }
            }
            Mouse::LeftUp | Mouse::RightUp => {
                if let Some(i) = self.ui.menu_item_at(w, h, x, y) {
                    let menu = self.ui.menu.take();
                    if let Some(m) = menu {
                        let cmd = m.items[i].cmd.clone();
                        match m.page {
                            Some(p) => {
                                // Built-ins run inside WebView2; ours run here.
                                if let crate::ui::MenuCmd::Builtin(c) | crate::ui::MenuCmd::Inspect(c) = cmd {
                                    unsafe {
                                        let _ = p.args.SetSelectedCommandId(c);
                                    }
                                    if matches!(cmd, crate::ui::MenuCmd::Inspect(_)) {
                                        self.wait_devtools(m.tab);
                                    }
                                } else {
                                    self.menu_command(m.tab, cmd);
                                }
                                unsafe {
                                    let _ = p.deferral.Complete();
                                }
                            }
                            None => self.menu_command(m.tab, cmd),
                        }
                    }
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    fn menu_command(&mut self, id: TabId, cmd: crate::ui::MenuCmd) {
        use crate::ui::MenuCmd::*;
        match cmd {
            RenameGroup(g) => return self.open_rename(g),
            DissolveGroup(g) => return self.dissolve_group(g),
            CloseGroup(g) => return self.close_group(g),
            RemoveBookmark(url) => {
                self.store.borrow_mut().toggle_bookmark(&url, "");
                return self.redraw();
            }
            ExtPick(i) => {
                let (x, y) = self.ui.menu_anchor;
                return self.open_extension_site_menu(i, x, y);
            }
            ExtSite(i, on) => return self.set_extension_site(i, on),
            Bar(op) => {
                self.bar_edit(op);
                return;
            }
            OpenDownload(i) => return self.open_download(i),
            Shortcut(action) => return self.run(action),
            OpenImage => {
                let src = self.context_image.clone();
                if src.starts_with("http") {
                    let id = self.add_tab(src, String::new(), false);
                    self.activate(id);
                }
                return;
            }
            DownloadImage => {
                let src = self.context_image.clone();
                let page = self.active_index().map(|i| self.tabs[i].url.clone()).unwrap_or_default();
                return self.download_direct(src, page);
            }
            DownloadMenu(i) => return self.open_download_menu(i),
            DownloadOpenWhenDone(i) => return self.toggle_open_when_done(i),
            DownloadPause(i) => return self.pause_download(i),
            DownloadCancel(i) => return self.cancel_download(i),
            DownloadsFolder => return self.open_downloads_folder(),
            AllDownloads => {
                let url = format!("{}#downloads", crate::app::SETTINGS_URL);
                match self.tabs.iter().find(|t| t.url.starts_with(crate::app::SETTINGS_URL)).map(|t| t.id) {
                    Some(id) => {
                        self.activate(id);
                        self.navigate_active(url);
                    }
                    None => self.open(url),
                }
                return;
            }
            ExtOpen(i) => return self.open_extension_options(i),
            ExtStore => return self.run_extensions_store(),
            ManageExtensions => {
                return self.manage_extensions();
            }
            OpenHere(url) => {
                self.navigate_active(url);
                return self.focus_page();
            }
            _ => {}
        }
        let Some(i) = self.tabs.iter().position(|t| t.id == id) else { return };
        match cmd {
            Pin => {
                self.tabs[i].pinned = !self.tabs[i].pinned;
                self.session_dirty = true;
            }
            Duplicate => {
                let (url, title) = (self.tabs[i].url.clone(), self.tabs[i].title.clone());
                self.activate(id);
                let new = self.add_tab(url, title, false);
                self.activate(new);
            }
            Reload => match self.tabs[i].view.as_ref() {
                Some(v) => unsafe {
                    let _ = v.webview.Reload();
                },
                None => self.activate(id),
            },
            CopyUrl => crate::app::copy_to_clipboard(self.hwnd, &self.tabs[i].url),
            NewWindow => self.detach(id, None),
            Close => self.close(id),
            CloseOthers => {
                let others: Vec<TabId> = self.tabs.iter().filter(|t| t.id != id && !t.pinned).map(|t| t.id).collect();
                self.activate(id);
                for o in others {
                    self.close(o);
                }
            }
            CloseBelow => {
                let below: Vec<TabId> = self.tabs[i + 1..].iter().filter(|t| !t.pinned).map(|t| t.id).collect();
                for b in below {
                    self.close(b);
                }
            }
            Builtin(_) | Inspect(_) => {}
            NewGroup => {
                let g = self.new_group(id);
                self.open_rename(g);
            }
            MoveToGroup(g) => self.set_group(id, Some(g)),
            Ungroup => self.set_group(id, None),
            RenameGroup(g) => self.open_rename(g),
            DissolveGroup(g) => self.dissolve_group(g),
            CloseGroup(g) => self.close_group(g),
            OpenTab(url) => {
                // Opens next to the current tab, in the background (like
                // every browser's "open in new tab").
                self.add_tab(url, String::new(), false);
                self.redraw();
            }
            OpenWindow(url) => {
                let private = self.private;
                let _ = crate::open_window(crate::WindowOpts { at: None, size: None, content: crate::Content::Urls(vec![url]), follow: None, private });
            }
            CopyText(text) => crate::app::copy_to_clipboard(self.hwnd, &text),
            SearchText(text) => {
                let url = self.settings.search_url.replace("{}", &crate::settings::url_encode(&text));
                let new = self.add_tab(url, String::new(), false);
                self.activate(new);
            }
            Back => self.run(Action::Back),
            Forward => self.run(Action::Forward),
            PageReload => self.run(Action::Reload),
            OpenHere(_) | RemoveBookmark(_) | ExtPick(_) | ExtSite(..) | ExtOpen(_) | ExtStore | ManageExtensions | Bar(_) | OpenDownload(_) | DownloadsFolder | AllDownloads | DownloadMenu(_) | DownloadOpenWhenDone(_) | DownloadPause(_) | DownloadCancel(_) | OpenImage | DownloadImage | Shortcut(_) => {}
        }
    }

    // ---------- tooltips ----------

    /// Shows the tooltip for the hovered control (called by TIMER_TIP).
    pub fn show_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
        }
        let Some(hit) = self.ui.hover else { return };
        // Bookmarks and pinned tiles show what they are (they have no label).
        let bookmark_title = match hit {
            Hit::Bookmark(i) => self.ui.bookmarks.get(i).map(|(t, u)| if t.is_empty() { u.clone() } else { t.clone() }),
            Hit::Tile(id) => self.tabs.iter().find(|t| t.id == id).map(|t| if t.title.is_empty() { crate::ui::short_url(&t.url) } else { t.title.clone() }),
            _ => None,
        };
        let Some((label, action)) = tip_for(hit) else { return };
        let label: String = bookmark_title.unwrap_or_else(|| label.to_string());
        let keys = action
            .and_then(|a| self.settings.shortcuts.0.get(&a).and_then(|k| k.first().cloned()))
            .map(|k| key_chips(&k))
            .unwrap_or_default();
        let tabs = views(&self.tabs, &self.icons, self.active);
        let Some((r, _)) = self.ui.layout(&tabs, self.height_dip()).into_iter().find(|(_, h)| *h == hit) else { return };
        let x = r.left + self.ui.offset();
        let mut t = Tween::new(0.0);
        t.go(1.0, if self.ui.anims { 120 } else { 60 });
        let (x, y) = if matches!(hit, Hit::Settings) { (x, r.top - 36.0) } else { (x, r.bottom + 6.0) };
        self.ui.tip = Some(Tip { label, keys, x, y, t });
        self.redraw();
    }

    fn hide_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_TIP);
        }
        self.ui.tip = None;
    }

    // ---------- command bar ----------

    /// Ctrl+T (new tab) / Ctrl+L or click on the address (current tab).
    pub fn open_bar(&mut self, new_tab: bool) {
        let url = if new_tab {
            String::new()
        } else {
            self.active_index().map(|i| self.tabs[i].url.clone()).unwrap_or_default()
        };
        let url = if url.is_empty() { String::new() } else { url };
        let bar = &mut self.ui.bar;
        bar.open = true;
        bar.new_tab = new_tab;
        bar.rename = None;
        bar.field.set(&url, true);
        bar.items.clear();
        bar.sel = 0;
        bar.hover = None;
        bar.caret_blink = Instant::now();
        bar.t.go(1.0, if self.ui.anims { 160 } else { 60 });
        self.hide_tip();
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_CARET, 530, None);
        }
        // Keys typed between Ctrl+T and now.
        crate::app::BAR_PENDING.with(|p| p.set(false));
        let ahead: Vec<u16> = crate::app::TYPED_AHEAD.with(|t| t.borrow_mut().drain(..).collect());
        if !ahead.is_empty() {
            self.ui.bar.field.set("", false);
            self.ui.bar.field.insert(&ahead);
            self.bar_changed();
        }
        if crate::app::ENTER_AHEAD.with(|e| e.replace(false)) {
            self.bar_key(0x0D);
        }
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.redraw();
    }

    pub fn close_bar(&mut self) {
        if !self.ui.bar.open {
            return;
        }
        self.ui.bar.open = false;
        self.ui.bar.t.go(0.0, if self.ui.anims { 120 } else { 60 });
        self.focus_page();
        self.redraw();
    }

    pub fn focus_page(&self) {
        if let Some(v) = self.active_index().and_then(|i| self.tabs[i].view.as_ref()) {
            unsafe {
                let _ = v.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        }
    }

    fn bar_changed(&mut self) {
        if self.ui.bar.rename.is_some() {
            self.ui.bar.caret_blink = Instant::now();
            self.redraw();
            return;
        }
        let text = self.ui.bar.field.string();
        let tabs: Vec<(u64, &str, &str)> = self
            .tabs
            .iter()
            .filter(|t| Some(t.id) != self.active)
            .map(|t| (t.id, t.title.as_str(), t.url.as_str()))
            .collect();
        let mut items = self.store.borrow().suggest(&text, &tabs, is_address(&text), store::now());
        // Commands come right after "go / search" (both languages always work).
        for (n, action) in command_matches(&text).into_iter().enumerate() {
            items.insert((1 + n).min(items.len()), Suggestion::Command(action));
        }
        self.ui.bar.items = items;
        self.ui.bar.sel = 0;
        self.ui.bar.caret_blink = Instant::now();
        self.redraw();
    }

    fn bar_mouse(&mut self, kind: Mouse, xd: f32, yd: f32) {
        let page = self.page_dip();
        match kind {
            Mouse::Move => {
                let row = self.ui.bar_row_at(page, xd, yd);
                if row != self.ui.bar.hover {
                    self.ui.bar.hover = row;
                    self.redraw();
                }
            }
            Mouse::RightUp if self.ui.in_bar(page, xd, yd) && self.ui.bar_row_at(page, xd, yd).is_none() => {
                self.open_bar_menu(xd, yd);
            }
            Mouse::LeftDown => {
                if let Some(i) = self.ui.bar_row_at(page, xd, yd) {
                    self.pick(i);
                } else if self.ui.in_bar(page, xd, yd) {
                    let i = self.ui.field_index_at(page, xd);
                    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                    let f = &mut self.ui.bar.field;
                    f.caret = i;
                    if !shift {
                        f.anchor = i;
                    }
                    self.ui.bar.caret_blink = Instant::now();
                    self.redraw();
                } else {
                    self.close_bar();
                }
            }
            _ => {}
        }
    }

    /// Key press while the command bar is open. Returns true if consumed.
    /// Cut / copy / paste / select all / undo in the bar's field.
    pub fn bar_edit(&mut self, op: crate::ui::BarEdit) -> bool {
        use crate::ui::BarEdit::*;
        let f = &mut self.ui.bar.field;
        let mut changed = false;
        match op {
            SelectAll => f.select_all(),
            Copy | Cut => {
                let text = f.selected_text();
                if !text.is_empty() {
                    crate::app::copy_to_clipboard(self.hwnd, &text);
                    if op == Cut {
                        f.key(Key::Delete, false, false);
                        changed = true;
                    }
                }
            }
            Paste | PasteGo => {
                if let Some(text) = crate::app::paste_from_clipboard(self.hwnd) {
                    let one_line: Vec<u16> = text.replace(['\r', '\n'], " ").encode_utf16().collect();
                    f.insert(&one_line);
                    changed = true;
                }
            }
            Undo => {
                f.undo();
                changed = true;
            }
        }
        self.ui.bar.caret_blink = Instant::now();
        if changed {
            self.bar_changed();
        }
        if op == PasteGo && !self.ui.bar.field.string().trim().is_empty() {
            // Same as Enter on the top suggestion (go or search).
            self.pick(0);
        }
        self.redraw();
        true
    }

    /// Right-click in the bar's field.
    fn open_bar_menu(&mut self, x: f32, y: f32) {
        use crate::i18n::t;
        use crate::ui::{BarEdit, MenuCmd, MenuItem};
        let has_sel = {
            let (a, b) = self.ui.bar.field.selection();
            a != b
        };
        let has_clip = crate::app::paste_from_clipboard(self.hwnd).is_some_and(|s| !s.trim().is_empty());
        let item = |label: &str, op, keys: &str, sep| MenuItem { label: label.into(), cmd: MenuCmd::Bar(op), keys: keys.into(), sep };
        let mut items = Vec::new();
        if has_sel {
            items.push(item(t("Taglia", "Cut"), BarEdit::Cut, "Ctrl+X", false));
            items.push(item(t("Copia", "Copy"), BarEdit::Copy, "Ctrl+C", false));
        }
        if has_clip {
            items.push(item(t("Incolla", "Paste"), BarEdit::Paste, "Ctrl+V", false));
            items.push(item(t("Incolla e vai", "Paste and go"), BarEdit::PasteGo, "", false));
        }
        items.push(item(t("Seleziona tutto", "Select all"), BarEdit::SelectAll, "Ctrl+A", !items.is_empty()));
        self.show_menu(items, x, y);
    }

    pub fn bar_key(&mut self, vk: u16) -> bool {
        if self.ui.menu.is_some() && vk == 0x1B {
            self.close_menu();
            return true;
        }
        if self.find_key(vk) {
            return true;
        }
        if self.peek.is_some() && vk == 0x1B {
            self.close_peek();
            return true;
        }
        if self.history_open() && vk == 0x1B {
            self.close_history();
            return true;
        }
        if !self.ui.bar.open {
            // Bar requested (Ctrl+T) but not open yet: keep editing keys.
            if crate::app::BAR_PENDING.with(|p| p.get()) {
                match vk {
                    0x0D => crate::app::ENTER_AHEAD.with(|e| e.set(true)),
                    0x08 => {
                        crate::app::TYPED_AHEAD.with(|t| t.borrow_mut().pop());
                    }
                    0x1B => {
                        crate::app::BAR_PENDING.with(|p| p.set(false));
                        crate::app::TYPED_AHEAD.with(|t| t.borrow_mut().clear());
                    }
                    _ => return false,
                }
                return true;
            }
            return false;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let f = &mut self.ui.bar.field;
        let mut changed = false;
        match vk {
            0x1B => self.close_bar(),                                   // Esc
            0x0D if self.ui.bar.rename.is_some() => {
                let g = self.ui.bar.rename.take().unwrap_or_default();
                let name = self.ui.bar.field.string();
                self.rename_group(g, &name);
                self.close_bar();
            }
            0x0D => {
                let i = self.ui.bar.sel;
                if self.ui.bar.items.is_empty() {
                    let text = self.ui.bar.field.string();
                    if !text.trim().is_empty() {
                        let url = self.settings.resolve_input(&text);
                        self.go(url);
                    }
                } else {
                    self.pick(i);
                }
            }
            0x26 | 0x28 => {
                // Up / Down
                let n = self.ui.bar.items.len().min(8);
                if n > 0 {
                    let d = if vk == 0x26 { n - 1 } else { 1 };
                    self.ui.bar.sel = (self.ui.bar.sel + d) % n;
                }
            }
            0x25 => f.key(Key::Left, ctrl, shift),
            0x27 => f.key(Key::Right, ctrl, shift),
            0x24 => f.key(Key::Home, ctrl, shift),
            0x23 => f.key(Key::End, ctrl, shift),
            0x08 => {
                f.key(Key::Back, ctrl, false);
                changed = true;
            }
            0x2E => {
                f.key(Key::Delete, ctrl, false);
                changed = true;
            }
            0x41 if ctrl => return self.bar_edit(crate::ui::BarEdit::SelectAll),
            0x43 if ctrl => return self.bar_edit(crate::ui::BarEdit::Copy),
            0x58 if ctrl => return self.bar_edit(crate::ui::BarEdit::Cut),
            0x56 if ctrl => return self.bar_edit(crate::ui::BarEdit::Paste),
            0x5A if ctrl => return self.bar_edit(crate::ui::BarEdit::Undo),
            // Other shortcuts keep working with the bar open.
            _ => return false,
        }
        self.ui.bar.caret_blink = Instant::now();
        if changed {
            self.bar_changed();
        } else {
            self.redraw();
        }
        true
    }

    /// Typed character while the command bar is open.
    pub fn bar_char(&mut self, ch: u16) -> bool {
        if self.find_char(ch) {
            return true;
        }
        if !self.ui.bar.open {
            if crate::app::BAR_PENDING.with(|p| p.get()) && ch >= 0x20 {
                crate::app::TYPED_AHEAD.with(|t| t.borrow_mut().push(ch));
                return true;
            }
            return false;
        }
        if ch >= 0x20 && ch != 0x7F {
            self.ui.bar.field.insert(&[ch]);
            self.bar_changed();
        }
        true
    }

    fn pick(&mut self, i: usize) {
        let Some(item) = self.ui.bar.items.get(i).cloned() else { return };
        match item {
            Suggestion::Tab { id, .. } => {
                self.close_bar();
                self.activate(id);
            }
            Suggestion::Bookmark { url, .. } | Suggestion::History { url, .. } => self.go(url),
            Suggestion::Go { input, .. } => {
                let url = self.settings.resolve_input(&input);
                self.go(url);
            }
            Suggestion::Command(action) => {
                self.close_bar();
                self.run(action);
            }
        }
    }

    /// Opens `url` from the command bar: new tab (Ctrl+T) or current tab.
    fn go(&mut self, url: String) {
        let new_tab = self.ui.bar.new_tab || self.active.is_none();
        self.ui.bar.open = false;
        self.ui.bar.t.go(0.0, if self.ui.anims { 120 } else { 60 });
        if new_tab {
            self.open(url);
        } else {
            self.navigate_active(url);
        }
        self.focus_page();
        self.redraw();
    }
}

/// The Veyra window visible under `pt`, looking past `own` (the torn-off
/// window being dragged, which is under the pointer itself).
/// Command bar commands and the words that find them, in Italian and in
/// English whatever the language of the app.
const COMMANDS: &[(Action, &[&str])] = &[(
    Action::PrivateWindow,
    &["privata", "finestra privata", "nuova finestra privata", "navigazione privata", "private", "private window", "new private window", "private browsing"],
)];

/// Commands whose words start with what is typed (at least 3 letters).
fn command_matches(text: &str) -> Vec<Action> {
    let q = text.trim().to_lowercase();
    if q.chars().count() < 3 {
        return Vec::new();
    }
    COMMANDS.iter().filter(|(_, words)| words.iter().any(|w| w.starts_with(&q))).map(|(a, _)| *a).collect()
}

/// The Veyra window under `pt` (not `own`), if it is of the same kind:
/// tabs never move between private and normal windows.
pub(crate) fn browser_window_under(pt: POINT, own: windows::Win32::Foundation::HWND, private: bool) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    unsafe {
        let mut h = GetTopWindow(None).ok()?;
        loop {
            let mut r = RECT::default();
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
            let click_through = GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0;
            if h != own
                && IsWindowVisible(h).as_bool()
                && !IsIconic(h).as_bool()
                && cloaked == 0
                && !click_through
                && GetWindowRect(h, &mut r).is_ok()
                && pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
            {
                let mut class = [0u16; 32];
                let n = GetClassNameW(h, &mut class);
                let is_browser = String::from_utf16_lossy(&class[..n.max(0) as usize]) == "VeyraMain";
                let same = is_browser && crate::with_window(h, |a| a.private == private).unwrap_or(false);
                return same.then_some(h);
            }
            h = GetWindow(h, GW_HWNDNEXT).ok()?;
        }
    }
}
/// Tooltip label and the action whose shortcut it shows.
fn tip_for(hit: Hit) -> Option<(&'static str, Option<Action>)> {
    use crate::i18n::t;
    Some(match hit {
        Hit::Back => (t("Indietro", "Back"), Some(Action::Back)),
        Hit::Forward => (t("Avanti", "Forward"), Some(Action::Forward)),
        Hit::Reload => (t("Ricarica", "Reload"), Some(Action::Reload)),
        Hit::Url => (t("Cerca o vai a un indirizzo", "Search or go to an address"), Some(Action::FocusAddress)),
        Hit::NewTab => (t("Nuova scheda", "New tab"), Some(Action::NewTab)),
        Hit::Settings => (t("Impostazioni", "Settings"), Some(Action::Settings)),
        Hit::Downloads => (t("Download", "Downloads"), None),
        Hit::CloseTab(_) => (t("Chiudi scheda", "Close tab"), Some(Action::CloseTab)),
        Hit::Minimize => (t("Riduci a icona", "Minimize"), None),
        Hit::Maximize => (t("Ingrandisci", "Maximize"), None),
        Hit::Close => (t("Chiudi", "Close"), None),
        Hit::Star => (t("Aggiungi o togli dai preferiti", "Add or remove bookmark"), Some(Action::Bookmark)),
        Hit::Bookmark(_) | Hit::Tile(_) => ("", None),
        Hit::MoreBookmarks => (t("Altri preferiti", "More bookmarks"), None),
        Hit::Extensions => (t("Estensioni", "Extensions"), None),
        _ => return None,
    })
}

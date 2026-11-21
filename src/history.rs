//! Ctrl+H: the history panel, a large glass panel on the left of the page.
//!
//! Its own WebView (ui/history.html, transparent background) in the `peek`
//! composition layer, like the link preview: the page behind is blurred and
//! dimmed, so the panel's translucent background reads as glass. The page
//! asks for visits and deletes them with JSON messages (`history_message`).
//!
//! Motion: nothing moves until the page has drawn its list ("ready"); then
//! one tween (`ui.history_t`) drives the blur, the dimming and the panel's
//! fade and slide together, every frame (shell.rs). Closed, the WebView is
//! kept hidden, so opening it again is immediate.

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::Result;
use windows::{
    Win32::{Foundation::RECT, UI::WindowsAndMessaging::HCURSOR},
    core::*,
};

use crate::{
    app::{App, Event, INTERNAL_HOST, TabId, View, post},
    shortcuts::Action,
};

/// Pseudo tab id for the panel's WebView events.
pub const HISTORY_ID: TabId = u64::MAX - 1;
const PAGE: &str = "history.html";
const ICONS_HOST: &str = "veyra.favicons";
/// Visits sent per request (the list loads more while scrolling).
const BATCH: usize = 150;

/// Tests: toggles the panel (no keyboard needed). Answers 1 when open.
pub const WM_TEST_HISTORY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 14;

/// How far the panel slides in, in DIPs.
const SLIDE: f32 = 18.0;

pub struct HistoryPanel {
    view: Option<View>,
    /// Open (false: closing, or kept hidden for next time).
    open: bool,
    /// In the composition layer now.
    shown: bool,
    /// Theme and language the page was made with.
    look: (bool, &'static str),
}

impl App {
    pub fn history_open(&self) -> bool {
        self.history_panel.as_ref().is_some_and(|h| h.open)
    }

    /// The panel's motion 0..1 while it is on screen (also while closing).
    pub fn history_progress(&self) -> Option<f32> {
        self.history_panel.as_ref().filter(|h| h.shown).map(|_| self.ui.history_t.value())
    }

    pub fn toggle_history(&mut self) {
        if self.history_open() { self.close_history() } else { self.open_history() }
    }

    pub fn open_history(&mut self) {
        if self.history_open() {
            return;
        }
        self.close_menu();
        self.close_peek(); // they share the layer
        self.close_bar();
        let look = (self.ui.theme.dark, crate::i18n::code());
        // The panel kept from last time: show it again (fresh list first).
        if let Some(h) = &mut self.history_panel
            && h.look == look
            && let Some(v) = &h.view
        {
            h.open = true;
            if !h.shown {
                h.shown = true;
                self.comp.show_peek(Some(&v.visual));
            }
            unsafe {
                let _ = v.controller.SetIsVisible(true);
                let _ = v.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
            self.history_reply(&serde_json::json!({ "type": "refresh" }));
            self.layout();
            self.redraw();
            return;
        }
        self.drop_history();
        self.ui.history_t.set(0.0);
        self.history_panel = Some(HistoryPanel { view: None, open: true, shown: false, look });
        let hwnd = self.hwnd;
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(move |err, c| {
            match (err, c) {
                (Ok(()), Some(c)) => post(hwnd, Event::Created(HISTORY_ID, c)),
                _ => post(hwnd, Event::HistoryClose),
            }
            Ok(())
        }));
        unsafe {
            if let Ok(env3) = self.env.cast::<ICoreWebView2Environment3>() {
                let _ = env3.CreateCoreWebView2CompositionController(hwnd, &handler);
            }
        }
        self.layout();
        self.redraw();
    }

    pub fn history_created(&mut self, cc: ICoreWebView2CompositionController) {
        if !self.history_open() {
            // Closed before it was ready.
            if let Ok(c) = cc.cast::<ICoreWebView2Controller>() {
                unsafe {
                    let _ = c.Close();
                }
            }
            return;
        }
        let hwnd = self.hwnd;
        let keymap = self.keymap.clone();
        let dark = self.ui.theme.dark;
        let result: Result<View> = (|| unsafe {
            let controller: ICoreWebView2Controller = cc.cast()?;
            let visual = self.comp.tab_visual()?;
            cc.SetRootVisualTarget(&visual)?;
            let mut token = 0;
            cc.add_CursorChanged(
                &CursorChangedEventHandler::create(Box::new(move |sender, _| {
                    if let Some(c) = sender {
                        let mut h = HCURSOR::default();
                        c.Cursor(&mut h)?;
                        crate::app::page_cursor_changed(hwnd, h);
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            // Transparent: the blurred page shows through the panel.
            if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
                c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 0, R: 0, G: 0, B: 0 })?;
            }
            let webview = controller.CoreWebView2()?;
            let settings = webview.Settings()?;
            settings.SetIsStatusBarEnabled(false)?;
            settings.SetAreDefaultContextMenusEnabled(false)?;
            settings.SetAreDevToolsEnabled(false)?;
            settings.SetIsZoomControlEnabled(false)?;
            if let Ok(wv3) = webview.cast::<ICoreWebView2_3>() {
                wv3.SetVirtualHostNameToFolderMapping(
                    &HSTRING::from(INTERNAL_HOST),
                    &HSTRING::from(crate::app::ui_dir().as_os_str()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
                )?;
                // Site icons saved by the sidebar.
                wv3.SetVirtualHostNameToFolderMapping(
                    &HSTRING::from(ICONS_HOST),
                    &HSTRING::from(self.data_dir.join("favicons").as_os_str()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
                )?;
            }
            // Esc, or the history shortcut again, closes it.
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_UP;
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN {
                        return Ok(());
                    }
                    let mut vk = 0u32;
                    args.VirtualKey(&mut vk)?;
                    if vk == 0x1B || keymap.borrow().lookup(vk as u16) == Some(Action::History) {
                        args.SetHandled(true)?;
                        post(hwnd, Event::HistoryClose);
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            // Only our own page talks to the host.
            webview.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut p = PWSTR::null();
                    args.Source(&mut p)?;
                    if !CoTaskMemPWSTR::from(p).to_string().starts_with(&format!("https://{INTERNAL_HOST}/{PAGE}")) {
                        return Ok(());
                    }
                    let mut p = PWSTR::null();
                    args.WebMessageAsJson(&mut p)?;
                    post(hwnd, Event::HistoryMsg(CoTaskMemPWSTR::from(p).to_string()));
                    Ok(())
                })),
                &mut token,
            )?;
            controller.SetIsVisible(true)?;
            let url = format!("https://{INTERNAL_HOST}/{PAGE}?l={}&d={}", crate::i18n::code(), dark as u8);
            webview.Navigate(&HSTRING::from(url))?;
            controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)?;
            Ok(View { controller, comp: cc.clone(), webview, visual })
        })();
        match result {
            Ok(view) => {
                // In the layer but invisible (progress 0) until "ready".
                self.comp.show_peek(Some(&view.visual));
                if let Some(h) = &mut self.history_panel {
                    h.view = Some(view);
                    h.shown = true;
                }
                self.layout();
                self.redraw();
            }
            Err(e) => {
                eprintln!("history: {e}");
                self.drop_history();
            }
        }
    }

    /// Slides the panel out; it stays hidden for next time (`settle_history`).
    pub fn close_history(&mut self) {
        let Some(h) = &mut self.history_panel else { return };
        if !h.open {
            return;
        }
        h.open = false;
        if h.view.is_none() {
            // Still being created: nothing on screen yet.
            return self.drop_history();
        }
        let ms = if self.ui.anims { 170 } else { 60 };
        self.ui.history_t.go(0.0, ms);
        self.focus_page();
        self.redraw();
    }

    /// Off screen at once (the link preview takes the layer).
    pub fn hide_history_now(&mut self) {
        if let Some(h) = &mut self.history_panel {
            h.open = false;
        }
        self.ui.history_t.set(0.0);
        self.settle_history();
    }

    /// After a frame: once the closing slide ended, out of the layer, hidden.
    pub fn settle_history(&mut self) {
        let Some(h) = &mut self.history_panel else { return };
        if h.open || !h.shown || self.ui.history_t.running() {
            return;
        }
        h.shown = false;
        if let Some(v) = &h.view {
            self.comp.hide_peek(&v.visual);
            unsafe {
                let _ = v.controller.SetIsVisible(false);
            }
        }
        self.redraw();
    }

    /// Destroys the panel's WebView (failed, or made with another theme).
    fn drop_history(&mut self) {
        if let Some(h) = self.history_panel.take() {
            if let Some(v) = h.view {
                if h.shown {
                    self.comp.hide_peek(&v.visual);
                }
                unsafe {
                    let _ = v.controller.Close();
                }
            }
        }
        self.ui.history_t.set(0.0);
        self.redraw();
    }

    /// The panel in client px: on the left of the page, its full height.
    pub fn history_rect(&self) -> Option<RECT> {
        self.history_panel.as_ref().filter(|h| h.shown)?;
        let p = self.page_rect();
        let s = self.scale;
        let gap = (12.0 * s) as i32;
        let pw = (p.right - p.left) as f32 / s;
        let w = ((pw * 0.42).clamp(440.0, 620.0).min(pw - 24.0) * s) as i32;
        Some(RECT { left: p.left + gap, top: p.top + gap, right: p.left + gap + w, bottom: p.bottom - gap })
    }

    pub fn place_history(&self) {
        let Some(r) = self.history_rect() else { return };
        if let Some(v) = self.history_panel.as_ref().and_then(|h| h.view.as_ref()) {
            unsafe {
                let _ = v.controller.SetBounds(RECT { left: 0, top: 0, right: r.right - r.left, bottom: r.bottom - r.top });
            }
        }
        self.move_history();
    }

    /// Each frame: the panel's slide and fade for the current progress.
    pub fn move_history(&self) {
        let (Some(r), Some(t)) = (self.history_rect(), self.history_progress()) else { return };
        let dx = (-(1.0 - t) * SLIDE * self.scale).round() as i32;
        if crate::sys::perf_on() && self.ui.history_t.running() {
            eprintln!("[history] {:?} t={t:.3}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        }
        let moved = RECT { left: r.left + dx, right: r.right + dx, ..r };
        self.comp.set_peek_rect(moved, 18.0 * self.scale);
        self.comp.set_peek_opacity(t);
    }

    /// Mouse while the panel is open: inside goes to it, a click outside
    /// closes it. Always consumed.
    pub fn history_mouse(&mut self, kind: crate::shell::Mouse, x: i32, y: i32, keys: u32, data: u32) {
        let Some(r) = self.history_rect() else { return };
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            if let Some(cc) = self.history_panel.as_ref().and_then(|h| h.view.as_ref()).map(|v| v.comp.clone()) {
                self.forward_to(&cc, r, kind, x, y, keys, data);
            }
        } else if kind == crate::shell::Mouse::LeftDown {
            self.close_history();
        }
    }

    /// Messages from ui/history.html.
    pub fn history_message(&mut self, json: &str) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(json) else { return };
        let num = |k: &str| msg.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        match msg.get("type").and_then(|t| t.as_str()) {
            // A page of visits, newest first, matching `q`, after the first `skip`.
            // `i`: the site's icon in https://veyra.favicons/ (if saved).
            Some("list") => {
                let q = msg.get("q").and_then(|q| q.as_str()).unwrap_or("").to_string();
                let skip = num("skip") as usize;
                let reply = {
                    let store = self.store.borrow();
                    let found = store.visits(&q, skip, BATCH + 1);
                    let more = found.len() > BATCH;
                    let items: Vec<_> = found
                        .into_iter()
                        .take(BATCH)
                        .map(|e| {
                            let icon = self.favicon_path(&crate::app::host_of(&e.url));
                            let icon = icon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            serde_json::json!({ "u": e.url, "t": e.title, "a": e.at, "i": icon })
                        })
                        .collect();
                    serde_json::json!({ "type": "list", "q": q, "skip": skip, "more": more, "items": items })
                };
                self.history_reply(&reply);
            }
            Some("open") => {
                if let Some(url) = msg.get("u").and_then(|u| u.as_str()).filter(|u| u.starts_with("http")) {
                    let id = self.add_tab(url.to_string(), String::new(), false);
                    self.close_history();
                    self.activate(id);
                }
            }
            Some("delete") => {
                let visits: Vec<(String, u64)> = msg
                    .get("items")
                    .and_then(|i| i.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| Some((v.get("u")?.as_str()?.to_string(), v.get("a")?.as_u64()?)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.store.borrow_mut().delete_visits(&visits);
                self.history_reply(&serde_json::json!({ "type": "changed" }));
            }
            // A day, the last hour, today or everything: [from, to).
            Some("delete_range") => {
                let to = msg.get("to").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
                self.store.borrow_mut().delete_range(num("from"), to);
                self.history_reply(&serde_json::json!({ "type": "changed" }));
            }
            Some("close") => self.close_history(),
            // The list is drawn: now it can come in.
            Some("ready") => {
                if self.history_open() {
                    // One frame at 0 first: the dimming surface gets made
                    // now, not in the first frame of the motion (it jumped).
                    self.render_now();
                    let ms = if self.ui.anims { 280 } else { 60 };
                    self.ui.history_t.go(1.0, ms);
                    self.redraw();
                }
            }
            _ => {}
        }
    }

    fn history_reply(&self, reply: &serde_json::Value) {
        if let Some(v) = self.history_panel.as_ref().and_then(|h| h.view.as_ref()) {
            unsafe {
                let _ = v.webview.PostWebMessageAsJson(&HSTRING::from(reply.to_string()));
            }
        }
    }
}

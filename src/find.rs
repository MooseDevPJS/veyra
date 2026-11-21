//! Ctrl+F: find in page with Veyra's own bar (top right of the page), in
//! place of WebView2's built-in box. The search itself is the engine's
//! (`ICoreWebView2Find`, its dialog suppressed); the bar is drawn in the
//! overlay (ui.rs `draw_find`) and edited with the command bar's TextField.

use std::time::Instant;

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_SHIFT},
    core::{HSTRING, Interface},
};

use crate::{
    anim::Tween,
    app::{App, Event, Owner, TabId, post},
    shell::Mouse,
    textfield::Key,
    ui::{FindBar, FindHit},
};

impl App {
    /// Opens the bar (or puts the keyboard back in it, text selected).
    pub fn open_find(&mut self) {
        if self.active_index().and_then(|i| self.tabs[i].view.as_ref()).is_none() {
            return;
        }
        self.close_bar();
        let find = self.ui.find.get_or_insert_with(|| {
            // Fades in (the tween below starts it).
            let t = Tween::new(0.0);
            FindBar { field: Default::default(), count: (0, 0), focused: true, hover: None, t, caret_blink: Instant::now() }
        });
        find.focused = true;
        find.field.select_all();
        find.caret_blink = Instant::now();
        find.t.go(1.0, if self.ui.anims { 140 } else { 0 });
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
        self.find_changed();
    }

    /// Esc / X: clears the highlights and gives the keyboard back to the page.
    pub fn close_find(&mut self) {
        if self.ui.find.take().is_none() {
            return;
        }
        if let Some(f) = self.active_finder() {
            unsafe {
                let _ = f.Stop();
            }
        }
        self.focus_page();
        self.redraw();
    }

    /// The engine's finder for the active tab (hooked to report counts).
    fn active_finder(&mut self) -> Option<ICoreWebView2Find> {
        let i = self.active_index()?;
        let id = self.tabs[i].id;
        let webview = self.tabs[i].view.as_ref()?.webview.clone();
        let finder = unsafe { webview.cast::<ICoreWebView2_28>().ok()?.Find().ok()? };
        if self.find_hooked.insert(id) {
            let owner = Owner(id);
            let report = move |f: Option<ICoreWebView2Find>| {
                if let Some(f) = f {
                    let (mut active, mut total) = (0, 0);
                    unsafe {
                        let _ = f.ActiveMatchIndex(&mut active);
                        let _ = f.MatchCount(&mut total);
                    }
                    post(owner, Event::FindCount(id, active, total));
                }
                Ok(())
            };
            let mut token = 0;
            unsafe {
                let _ = finder.add_MatchCountChanged(&FindMatchCountChangedEventHandler::create(Box::new(move |f, _| report(f))), &mut token);
                let _ = finder.add_ActiveMatchIndexChanged(&FindActiveMatchIndexChangedEventHandler::create(Box::new(move |f, _| report(f))), &mut token);
            }
        }
        Some(finder)
    }

    /// The text changed: search again (empty: clear).
    fn find_changed(&mut self) {
        let Some(term) = self.ui.find.as_ref().map(|f| f.field.string()) else { return };
        let Some(finder) = self.active_finder() else { return };
        if term.is_empty() {
            unsafe {
                let _ = finder.Stop();
            }
            if let Some(f) = &mut self.ui.find {
                f.count = (0, 0);
            }
            return self.redraw();
        }
        let Ok(env) = self.env.cast::<ICoreWebView2Environment15>() else { return };
        unsafe {
            let Ok(options) = env.CreateFindOptions() else { return };
            let _ = options.SetFindTerm(&HSTRING::from(term));
            let _ = options.SetSuppressDefaultFindDialog(true);
            let _ = options.SetShouldHighlightAllMatches(true);
            let _ = finder.Start(&options, &FindStartCompletedHandler::create(Box::new(|_| Ok(()))));
        }
        self.redraw();
    }

    fn find_step(&mut self, back: bool) {
        if self.ui.find.as_ref().is_none_or(|f| f.field.text.is_empty()) {
            return;
        }
        if let Some(finder) = self.active_finder() {
            unsafe {
                let _ = if back { finder.FindPrevious() } else { finder.FindNext() };
            }
        }
    }

    /// The engine reported matches: "3/12".
    pub fn find_count(&mut self, id: TabId, active: i32, total: i32) {
        if self.active == Some(id)
            && let Some(f) = &mut self.ui.find
        {
            f.count = (if total > 0 { active + 1 } else { 0 }, total);
            self.redraw();
        }
    }

    /// Keys while the bar has the keyboard. False: not ours.
    pub fn find_key(&mut self, vk: u16) -> bool {
        if !self.ui.find.as_ref().is_some_and(|f| f.focused) {
            return false;
        }
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let Some(f) = &mut self.ui.find else { return false };
        let mut changed = false;
        match vk {
            0x1B => {
                self.close_find();
                return true;
            }
            0x0D => {
                self.find_step(shift); // Enter: next, Shift+Enter: previous
                return true;
            }
            0x72 => {
                self.find_step(shift); // F3 too
                return true;
            }
            0x25 => f.field.key(Key::Left, ctrl, shift),
            0x27 => f.field.key(Key::Right, ctrl, shift),
            0x24 => f.field.key(Key::Home, ctrl, shift),
            0x23 => f.field.key(Key::End, ctrl, shift),
            0x08 => {
                f.field.key(Key::Back, ctrl, false);
                changed = true;
            }
            0x2E => {
                f.field.key(Key::Delete, ctrl, false);
                changed = true;
            }
            0x41 if ctrl => f.field.select_all(),
            0x43 if ctrl => crate::app::copy_to_clipboard(self.hwnd, &f.field.selected_text()),
            0x56 if ctrl => {
                let text: Vec<u16> = crate::app::paste_from_clipboard(self.hwnd).unwrap_or_default().replace(['\r', '\n'], " ").encode_utf16().collect();
                f.field.insert(&text);
                changed = true;
            }
            0x5A if ctrl => {
                f.field.undo();
                changed = true;
            }
            // Other shortcuts (Ctrl+F again, Ctrl+T...) keep working.
            _ => return false,
        }
        if let Some(f) = &mut self.ui.find {
            f.caret_blink = Instant::now();
        }
        if changed { self.find_changed() } else { self.redraw() }
        true
    }

    /// A typed character for the bar. False: not ours.
    pub fn find_char(&mut self, ch: u16) -> bool {
        let Some(f) = self.ui.find.as_mut().filter(|f| f.focused) else { return false };
        if ch >= 0x20 && ch != 0x7F {
            f.field.insert(&[ch]);
            f.caret_blink = Instant::now();
            self.find_changed();
        }
        true
    }

    /// Mouse over the bar. True: consumed. A click anywhere else gives the
    /// keyboard back to the page (the bar stays open).
    pub fn find_mouse(&mut self, kind: Mouse, x: i32, y: i32) -> bool {
        let Some(find) = &self.ui.find else { return false };
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        let hit = self.ui.find_hit(self.page_dip(), xd, yd);
        if find.hover != hit.filter(|h| *h != FindHit::Field) {
            if let Some(f) = &mut self.ui.find {
                f.hover = hit.filter(|h| *h != FindHit::Field);
            }
            self.redraw();
        }
        let Some(hit) = hit else {
            if matches!(kind, Mouse::LeftDown | Mouse::RightDown)
                && let Some(f) = &mut self.ui.find
            {
                f.focused = false;
                self.redraw();
            }
            return false;
        };
        if kind == Mouse::LeftDown {
            match hit {
                FindHit::Field | FindHit::Panel => {
                    if let Some(f) = &mut self.ui.find {
                        f.focused = true;
                        f.caret_blink = Instant::now();
                    }
                    unsafe {
                        let _ = SetFocus(Some(self.hwnd));
                    }
                    self.redraw();
                }
                FindHit::Prev => self.find_step(true),
                FindHit::Next => self.find_step(false),
                FindHit::Close => self.close_find(),
            }
        }
        true
    }
}

//! Alt+click on a link: a floating preview of it over the current page.
//!
//! The preview is its own WebView (created on demand, destroyed on close),
//! shown in the `peek` composition layer as a rounded card. The page behind
//! is blurred and dimmed. "Open as tab" turns it into a real tab.

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::Result;
use windows::{
    Win32::{
        Foundation::RECT,
        Graphics::Direct2D::Common::D2D_RECT_F,
        UI::WindowsAndMessaging::HCURSOR,
    },
    core::*,
};

use crate::{
    app::{App, Event, TabId, View, post},
    gfx::rect,
};

/// Pseudo tab id used for the preview's WebView events.
pub const PEEK_ID: TabId = u64::MAX;

pub struct Peek {
    pub url: String,
    pub view: Option<View>,
    pub hover: Option<PeekButton>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PeekButton {
    OpenTab,
    Close,
}

/// Injected in every page: Alt+click on a link asks for a preview instead
/// of following it. A right-click also reports the image under the
/// pointer: the engine does not give its address for an image inside a
/// link or under an overlay (Pinterest...).
pub fn page_script(token: &str) -> String {
    format!(
        r#"(() => {{
  if (window.top !== window || !(window.chrome && chrome.webview)) return;
  document.addEventListener('click', (e) => {{
    if (!e.altKey || e.button !== 0) return;
    const a = e.target && e.target.closest && e.target.closest('a[href]');
    if (!a || !/^https?:/.test(a.href)) return;
    e.preventDefault(); e.stopPropagation();
    chrome.webview.postMessage({{ k: {token}, peek: a.href }});
  }}, true);
  document.addEventListener('contextmenu', (e) => {{
    let img = e.target && e.target.closest && e.target.closest('img');
    if (!img) img = document.elementsFromPoint(e.clientX, e.clientY).find((x) => x.tagName === 'IMG');
    const src = img ? (img.currentSrc || img.src || '') : '';
    chrome.webview.postMessage({{ k: {token}, ctximg: /^https?:/.test(src) ? src : '' }});
  }}, true);
}})();"#,
        token = serde_json::to_string(token).unwrap_or_default()
    )
}

impl App {
    pub fn open_peek(&mut self, url: String) {
        if !url.starts_with("http") {
            return;
        }
        self.close_menu();
        self.close_history(); // they share the layer
        if let Some(p) = &mut self.peek {
            p.url = url.clone();
            if let Some(v) = &p.view {
                unsafe {
                    let _ = v.webview.Navigate(&HSTRING::from(url));
                }
            }
            return;
        }
        self.peek = Some(Peek { url, view: None, hover: None });
        self.ui.bar.t.go(0.0, 0); // never both at once
        let hwnd = self.hwnd;
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(move |err, c| {
            match (err, c) {
                (Ok(()), Some(c)) => post(hwnd, Event::Created(PEEK_ID, c)),
                _ => post(hwnd, Event::PeekClose),
            }
            Ok(())
        }));
        unsafe {
            if let Ok(env3) = self.env.cast::<ICoreWebView2Environment3>() {
                let _ = env3.CreateCoreWebView2CompositionController(hwnd, &handler);
            }
        }
        self.layout();
    }

    pub fn peek_created(&mut self, cc: ICoreWebView2CompositionController) {
        let Some(url) = self.peek.as_ref().map(|p| p.url.clone()) else {
            // Closed before it was ready.
            if let Ok(c) = cc.cast::<ICoreWebView2Controller>() {
                unsafe {
                    let _ = c.Close();
                }
            }
            return;
        };
        let hwnd = self.hwnd;
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
            let webview = controller.CoreWebView2()?;
            let settings = webview.Settings()?;
            settings.SetIsStatusBarEnabled(false)?;
            if let Some(b) = self.blocker.clone() {
                crate::shield::install(&self.env, &webview, b, std::rc::Rc::new(crate::shield::ShieldState::default()))?;
            }
            // Esc closes the preview even while it has the keyboard.
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut vk = 0u32;
                    args.VirtualKey(&mut vk)?;
                    if vk == 0x1B {
                        args.SetHandled(true)?;
                        post(hwnd, Event::PeekClose);
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            // Links opening new windows from the preview become tabs.
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        let mut handled = windows::core::BOOL(0);
                        args.Handled(&mut handled)?;
                        let mut user = windows::core::BOOL(0);
                        args.IsUserInitiated(&mut user)?;
                        if handled.as_bool() || !user.as_bool() {
                            args.SetHandled(true)?;
                            return Ok(());
                        }
                        let mut p = PWSTR::null();
                        args.Uri(&mut p)?;
                        let deferral = args.GetDeferral()?;
                        post(hwnd, Event::NewWindow(CoTaskMemPWSTR::from(p).to_string(), args, deferral));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            controller.SetIsVisible(true)?;
            webview.Navigate(&HSTRING::from(url.as_str()))?;
            controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)?;
            Ok(View { controller, comp: cc.clone(), webview, visual })
        })();
        match result {
            Ok(view) => {
                self.comp.show_peek(Some(&view.visual));
                if let Some(p) = &mut self.peek {
                    p.view = Some(view);
                }
                self.layout();
            }
            Err(e) => {
                eprintln!("peek: {e}");
                self.close_peek();
            }
        }
    }

    pub fn close_peek(&mut self) {
        if let Some(p) = self.peek.take() {
            if let Some(v) = p.view {
                unsafe {
                    let _ = v.controller.Close();
                }
            }
            self.comp.show_peek(None);
            self.focus_page();
            self.layout();
        }
    }

    /// "Open as tab": the preview's page becomes a tab next to the current one.
    pub fn peek_to_tab(&mut self) {
        let url = self.peek.as_ref().map(|p| {
            p.view
                .as_ref()
                .and_then(|v| {
                    let mut s = PWSTR::null();
                    unsafe { v.webview.Source(&mut s) }.ok()?;
                    Some(CoTaskMemPWSTR::from(s).to_string())
                })
                .unwrap_or_else(|| p.url.clone())
        });
        self.close_peek();
        if let Some(url) = url {
            let id = self.add_tab(url, String::new(), false);
            self.activate(id);
        }
    }

    /// Preview card in client px (centered over the page).
    pub fn peek_rect(&self) -> Option<RECT> {
        self.peek.as_ref()?;
        let p = self.page_rect();
        let (pw, ph) = (p.right - p.left, p.bottom - p.top);
        let w = (pw as f32 * 0.86) as i32;
        let top_gap = (52.0 * self.scale) as i32;
        let bottom_gap = (16.0 * self.scale) as i32;
        let left = p.left + (pw - w) / 2;
        Some(RECT { left, top: p.top + top_gap, right: left + w, bottom: p.top + ph - bottom_gap })
    }

    /// Button strip above the card (DIPs): (open-as-tab, close).
    pub fn peek_buttons(&self) -> Option<(D2D_RECT_F, D2D_RECT_F)> {
        let r = self.peek_rect()?;
        let s = self.scale;
        let (right, y) = (r.right as f32 / s, r.top as f32 / s - 40.0);
        let close = rect(right - 32.0, y, right, y + 32.0);
        let w = crate::ui::text_pill_width(crate::i18n::t("Apri in una scheda", "Open as tab"));
        let open = rect(close.left - 8.0 - w, y, close.left - 8.0, y + 32.0);
        Some((open, close))
    }

    pub fn place_peek(&self) {
        let Some(r) = self.peek_rect() else { return };
        self.comp.set_peek_rect(r, crate::ui::PAGE_RADIUS * self.scale);
        if let Some(v) = self.peek.as_ref().and_then(|p| p.view.as_ref()) {
            unsafe {
                let _ = v.controller.SetBounds(RECT { left: 0, top: 0, right: r.right - r.left, bottom: r.bottom - r.top });
            }
        }
    }

    /// Mouse while a preview is open. Returns true if consumed.
    pub fn peek_mouse(&mut self, kind: crate::shell::Mouse, x: i32, y: i32, keys: u32, data: u32) -> bool {
        use crate::shell::Mouse;
        let Some(r) = self.peek_rect() else { return false };
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        let buttons = self.peek_buttons();
        let hover = buttons.and_then(|(o, c)| {
            if crate::gfx::contains(&o, xd, yd) {
                Some(PeekButton::OpenTab)
            } else if crate::gfx::contains(&c, xd, yd) {
                Some(PeekButton::Close)
            } else {
                None
            }
        });
        if let Some(p) = &mut self.peek
            && p.hover != hover
        {
            p.hover = hover;
            self.redraw();
        }
        let inside = x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if inside {
            if let Some(cc) = self.peek.as_ref().and_then(|p| p.view.as_ref()).map(|v| v.comp.clone()) {
                self.forward_to(&cc, r, kind, x, y, keys, data);
            }
            return true;
        }
        if kind == Mouse::LeftDown {
            match hover {
                Some(PeekButton::OpenTab) => self.peek_to_tab(),
                Some(PeekButton::Close) | None => self.close_peek(),
            }
        }
        true
    }
}

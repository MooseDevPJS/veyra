//! Drag & drop from other apps (files, links, text).
//!
//! With visual hosting WebView2 does not register as a drop target, so the
//! window does: over the page the drag is forwarded to the WebView (file
//! uploads, dropping text into fields); over the sidebar a dropped file or
//! link opens in a new tab.

use windows::{
    Win32::{
        Foundation::{POINT, POINTL},
        System::{
            Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL},
            Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, ReleaseStgMedium},
            SystemServices::MODIFIERKEYS_FLAGS,
        },
        UI::Shell::{DragQueryFileW, HDROP},
    },
    core::{Ref, Result, implement},
};

#[implement(IDropTarget)]
pub struct DropTarget(pub windows::Win32::Foundation::HWND);

/// Where a drag currently is.
#[derive(Clone, Copy, PartialEq)]
pub enum Over {
    Page,
    Sidebar,
    Nothing,
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let data = data.cloned();
        let eff = crate::with_window(self.0, |a| a.drop_event(DropEvent::Enter(data), keys.0, *pt)).unwrap_or(DROPEFFECT_NONE);
        unsafe { *effect = eff };
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let eff = crate::with_window(self.0, |a| a.drop_event(DropEvent::Over, keys.0, *pt)).unwrap_or(DROPEFFECT_NONE);
        unsafe { *effect = eff };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        crate::with_window(self.0, |a| a.drop_event(DropEvent::Leave, 0, POINTL::default()));
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let data = data.cloned();
        let eff = crate::with_window(self.0, |a| a.drop_event(DropEvent::Drop(data), keys.0, *pt)).unwrap_or(DROPEFFECT_NONE);
        unsafe { *effect = eff };
        Ok(())
    }
}

pub enum DropEvent {
    Enter(Option<IDataObject>),
    Over,
    Leave,
    Drop(Option<IDataObject>),
}

impl crate::app::App {
    /// Routes a drag to the page (WebView2) or handles it on the sidebar.
    pub fn drop_event(&mut self, ev: DropEvent, keys: u32, pt: POINTL) -> DROPEFFECT {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        use windows::core::Interface;
        let mut client = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut client);
        }
        let p = self.page_rect();
        let over_page = client.x >= p.left && client.x < p.right && client.y >= p.top && client.y < p.bottom;
        let over = match ev {
            DropEvent::Leave => Over::Nothing,
            _ if over_page => Over::Page,
            _ => Over::Sidebar,
        };
        let cc3 = self
            .active_index()
            .and_then(|i| self.tabs[i].view.as_ref())
            .and_then(|v| v.comp.cast::<ICoreWebView2CompositionController3>().ok());
        let rel = POINT { x: client.x - p.left, y: client.y - p.top };

        // Leaving the page (to the sidebar or out): tell the WebView.
        if self.drag_over == Over::Page && over != Over::Page
            && let Some(cc) = &cc3
        {
            unsafe {
                let _ = cc.DragLeave();
            }
        }
        let entering_page = over == Over::Page && self.drag_over != Over::Page;
        self.drag_over = over;
        if let DropEvent::Enter(d) = &ev {
            self.drag_data = d.clone();
        }

        match (over, ev) {
            (Over::Page, ev) => {
                let Some(cc) = cc3 else { return DROPEFFECT_NONE };
                let mut effect = DROPEFFECT_COPY.0;
                unsafe {
                    match ev {
                        DropEvent::Drop(d) => {
                            let data = d.or_else(|| self.drag_data.clone());
                            if entering_page && let Some(dd) = &data {
                                let _ = cc.DragEnter(dd, keys, rel, &mut effect);
                            }
                            if let Some(dd) = &data {
                                let _ = cc.Drop(dd, keys, rel, &mut effect);
                            }
                            self.drag_over = Over::Nothing;
                            self.drag_data = None;
                        }
                        _ if entering_page => {
                            if let Some(dd) = &self.drag_data {
                                let _ = cc.DragEnter(dd, keys, rel, &mut effect);
                            }
                        }
                        _ => {
                            let _ = cc.DragOver(keys, rel, &mut effect);
                        }
                    }
                }
                DROPEFFECT(effect)
            }
            (Over::Sidebar, DropEvent::Drop(d)) => {
                let data = d.or_else(|| self.drag_data.take());
                self.drag_over = Over::Nothing;
                let urls = data.as_ref().map(dropped_urls).unwrap_or_default();
                for url in &urls {
                    let id = self.add_tab(url.clone(), String::new(), false);
                    self.activate(id);
                }
                if urls.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY }
            }
            (Over::Sidebar, _) => {
                let ok = self.drag_data.as_ref().is_some_and(|d| !dropped_urls(d).is_empty());
                if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE }
            }
            (Over::Nothing, _) => {
                self.drag_data = None;
                DROPEFFECT_NONE
            }
        }
    }
}

/// Files (as file:/// URLs) or a URL/text from a drag.
fn dropped_urls(data: &IDataObject) -> Vec<String> {
    const CF_HDROP: u16 = 15;
    const CF_UNICODETEXT: u16 = 13;
    let fmt = |cf: u16| FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut out = Vec::new();
    unsafe {
        if let Ok(mut medium) = data.GetData(&fmt(CF_HDROP)) {
            let hdrop = HDROP(medium.u.hGlobal.0);
            let n = DragQueryFileW(hdrop, u32::MAX, None);
            for i in 0..n.min(20) {
                let mut buf = [0u16; 1024];
                let len = DragQueryFileW(hdrop, i, Some(&mut buf));
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                out.push(format!("file:///{}", path.replace('\\', "/")));
            }
            ReleaseStgMedium(&mut medium);
        } else if let Ok(mut medium) = data.GetData(&fmt(CF_UNICODETEXT)) {
            let mem = medium.u.hGlobal;
            let ptr = windows::Win32::System::Memory::GlobalLock(mem) as *const u16;
            if !ptr.is_null() {
                let mut len = 0;
                while *ptr.add(len) != 0 && len < 8192 {
                    len += 1;
                }
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)).trim().to_string();
                let _ = windows::Win32::System::Memory::GlobalUnlock(mem);
                if text.starts_with("http://") || text.starts_with("https://") {
                    out.push(text);
                }
            }
            ReleaseStgMedium(&mut medium);
        }
    }
    out
}

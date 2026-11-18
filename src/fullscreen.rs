//! Page fullscreen (a video's fullscreen button, the Fullscreen API): the
//! window covers its whole monitor and the page fills it, with no sidebar,
//! inset or rounded corners. Leaving it (Esc, the video's button, another
//! tab) puts the window back exactly as it was.

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::Win32::{
    Graphics::{
        Dwm::{DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute},
        Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow},
    },
    UI::WindowsAndMessaging::{
        GetWindowPlacement, HWND_TOP, SW_RESTORE, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPlacement,
        SetWindowPos, ShowWindow, WINDOWPLACEMENT,
    },
};

use crate::app::{App, TabId};

impl App {
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen.is_some()
    }

    /// The page of tab `id` entered or left fullscreen.
    pub fn page_fullscreen(&mut self, id: TabId, on: bool) {
        if on && self.active == Some(id) {
            self.enter_fullscreen();
        } else if !on {
            self.leave_fullscreen();
        }
    }

    fn enter_fullscreen(&mut self) {
        if self.fullscreen.is_some() {
            return;
        }
        self.close_menu();
        unsafe {
            let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            if GetWindowPlacement(self.hwnd, &mut wp).is_err() {
                return;
            }
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                return;
            }
            self.fullscreen = Some(wp);
            // A maximized window is trimmed by WM_NCCALCSIZE: restore first
            // so the client area is the whole window.
            let _ = ShowWindow(self.hwnd, SW_RESTORE);
            // No rounded corners and no Windows 11 window border: the page
            // must cover every pixel of the screen.
            let corners = DWMWCP_DONOTROUND;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corners as *const _ as *const _, 4);
            let none: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &none as *const _ as *const _, 4);
            let m = mi.rcMonitor;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_NOOWNERZORDER | SWP_FRAMECHANGED);
        }
        self.layout();
    }

    pub fn leave_fullscreen(&mut self) {
        let Some(wp) = self.fullscreen.take() else { return };
        unsafe {
            let corners = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corners as *const _ as *const _, 4);
            let default: u32 = 0xFFFF_FFFF; // DWMWA_COLOR_DEFAULT
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &default as *const _ as *const _, 4);
            let _ = SetWindowPlacement(self.hwnd, &wp);
            let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        }
        self.layout();
    }
}

/// Subscribes a page's fullscreen changes (posted as `Event::FullScreen`).
pub fn watch(webview: &ICoreWebView2, id: TabId, hwnd: crate::app::Owner) -> windows::core::Result<()> {
    let mut token = 0;
    unsafe {
        webview.add_ContainsFullScreenElementChanged(
            &webview2_com::ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(wv) = sender {
                    let mut on = windows::core::BOOL(0);
                    wv.ContainsFullScreenElement(&mut on)?;
                    crate::app::post(hwnd, crate::app::Event::FullScreen(id, on.as_bool()));
                }
                Ok(())
            })),
            &mut token,
        )
    }
}

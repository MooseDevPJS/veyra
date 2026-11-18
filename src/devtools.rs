//! DevTools docked inside the browser window.
//!
//! WebView2 can only open DevTools as a separate top-level window. We find
//! that window (owned by the WebView2 browser process, title "DevTools - …")
//! and re-parent it as a child pane on the right of the page. It belongs to
//! one tab: hidden when another tab is active, gone when it is closed.

use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, RECT},
        UI::WindowsAndMessaging::*,
    },
    core::BOOL,
};

use crate::app::{App, TabId};

pub const TIMER_DEVTOOLS: usize = 7;
pub const HEADER: f32 = 34.0;

pub struct Dock {
    /// The DevTools window (other process).
    pub hwnd: HWND,
    /// Our clipping container; DevTools sits inside, shifted up so its own
    /// title bar is cut off.
    pub host: HWND,
    pub tab: TabId,
}

/// DevTools' own title bar height (DIPs), hidden by the container.
const DEVTOOLS_CAPTION: f32 = 32.0;
/// Posted to the main window when the docked DevTools is destroyed.
pub const WM_DOCK_GONE: u32 = WM_APP + 2;

extern "system" fn host_proc(hwnd: HWND, msg: u32, w: windows::Win32::Foundation::WPARAM, l: LPARAM) -> windows::Win32::Foundation::LRESULT {
    unsafe {
        if msg == WM_PARENTNOTIFY && (w.0 & 0xffff) as u32 == WM_DESTROY
            && let Ok(main) = GetParent(hwnd)
        {
            let _ = PostMessageW(Some(main), WM_DOCK_GONE, Default::default(), l);
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}

fn create_host(parent: HWND) -> Option<HWND> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: instance.into(),
            lpszClassName: windows::core::w!("VeyraDock"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("VeyraDock"),
            windows::core::w!(""),
            WS_CHILD | WS_CLIPCHILDREN,
            0,
            0,
            10,
            10,
            Some(parent),
            None,
            Some(instance.into()),
            None,
        )
        .ok()
    }
}

/// Waiting for the DevTools window of `tab` to appear.
pub struct Pending {
    pub tab: TabId,
    pub pid: u32,
    pub since: Instant,
}

impl App {
    /// F12 / Ctrl+Shift+I: open (docked) or close DevTools for the active tab.
    pub fn toggle_devtools(&mut self) {
        let Some(id) = self.active else { return };
        if self.dock.as_ref().is_some_and(|d| d.tab == id) {
            self.close_devtools();
            return;
        }
        if let Some(wv) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.view.as_ref()).map(|v| v.webview.clone()) {
            unsafe {
                let _ = wv.OpenDevToolsWindow();
            }
            self.wait_devtools(id);
        }
    }

    /// DevTools is being opened by WebView2 (also "Inspect"): dock it when
    /// its window shows up.
    pub fn wait_devtools(&mut self, id: TabId) {
        let Some(wv) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.view.as_ref()).map(|v| v.webview.clone()) else {
            return;
        };
        let mut pid = 0u32;
        unsafe {
            let _ = wv.BrowserProcessId(&mut pid);
        }
        // One docked DevTools at a time.
        if self.dock.as_ref().is_some_and(|d| d.tab != id) {
            self.close_devtools();
        }
        self.dock_wait = Some(Pending { tab: id, pid, since: Instant::now() });
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_DEVTOOLS, 60, None);
        }
    }

    /// TIMER_DEVTOOLS: look for the window and dock it.
    pub fn poll_devtools(&mut self) {
        let Some(p) = &self.dock_wait else {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_DEVTOOLS);
            }
            return;
        };
        if let Some(dev) = find_devtools_window(p.pid, self.dock.as_ref().map(|d| d.hwnd)) {
            let tab = p.tab;
            self.dock_wait = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_DEVTOOLS);
            }
            self.dock_window(dev, tab);
        } else if p.since.elapsed() > Duration::from_secs(5) {
            self.dock_wait = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_DEVTOOLS);
            }
        }
    }

    fn dock_window(&mut self, dev: HWND, tab: TabId) {
        // Already docked (Inspect while open): nothing to re-parent.
        if self.dock.as_ref().is_some_and(|d| d.hwnd == dev) {
            return;
        }
        let Some(host) = create_host(self.hwnd) else { return };
        unsafe {
            let style = GetWindowLongPtrW(dev, GWL_STYLE) as u32;
            let strip = (WS_POPUP | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX).0;
            SetWindowLongPtrW(dev, GWL_STYLE, ((style & !strip) | WS_CHILD.0 | WS_CLIPSIBLINGS.0) as isize);
            let ex = GetWindowLongPtrW(dev, GWL_EXSTYLE) as u32;
            let strip_ex = (WS_EX_APPWINDOW | WS_EX_WINDOWEDGE | WS_EX_DLGMODALFRAME | WS_EX_NOPARENTNOTIFY).0;
            SetWindowLongPtrW(dev, GWL_EXSTYLE, (ex & !strip_ex) as isize);
            let _ = SetParent(dev, Some(host));
            // Recompute the frame so the removed caption really disappears.
            let _ = SetWindowPos(dev, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        self.dock = Some(Dock { hwnd: dev, host, tab });
        self.layout();
        unsafe {
            let _ = ShowWindow(dev, SW_SHOW);
        }
    }

    pub fn close_devtools(&mut self) {
        if let Some(d) = self.dock.take() {
            unsafe {
                let _ = PostMessageW(Some(d.hwnd), WM_CLOSE, Default::default(), Default::default());
                let _ = ShowWindow(d.host, SW_HIDE);
                let _ = PostMessageW(Some(d.host), WM_CLOSE, Default::default(), Default::default());
            }
            self.layout();
        }
    }

    /// WM_PARENTNOTIFY(WM_DESTROY): the docked window went away (DevTools
    /// closed, or its tab was closed/unloaded).
    pub fn child_destroyed(&mut self, child: HWND) {
        if let Some(d) = self.dock.as_ref().filter(|d| d.hwnd == child) {
            unsafe {
                let _ = DestroyWindow(d.host);
            }
            self.dock = None;
            self.layout();
        }
    }

    /// Safety net (tick): the docked window vanished without notice.
    pub fn check_dock(&mut self) {
        if let Some(d) = &self.dock
            && !unsafe { IsWindow(Some(d.hwnd)) }.as_bool()
        {
            unsafe {
                let _ = DestroyWindow(d.host);
            }
            self.dock = None;
            self.layout();
        }
    }

    /// Docked pane visible right now (it belongs to the active tab).
    pub fn dock_visible(&self) -> bool {
        self.dock.as_ref().is_some_and(|d| Some(d.tab) == self.active)
    }

    /// Positions (or hides) the docked window. `pane` is in client px and
    /// includes the header strip we draw ourselves.
    pub fn place_dock(&self, pane: RECT) {
        let Some(d) = &self.dock else { return };
        unsafe {
            if !self.dock_visible() || !IsWindow(Some(d.hwnd)).as_bool() {
                let _ = ShowWindow(d.host, SW_HIDE);
                return;
            }
            let header = (HEADER * self.scale).round() as i32;
            let (w, h) = (pane.right - pane.left, (pane.bottom - pane.top - header).max(1));
            let _ = SetWindowPos(d.host, Some(HWND_TOP), pane.left, pane.top + header, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            // DevTools inside the host, its title bar above the visible area.
            let cap = (DEVTOOLS_CAPTION * self.scale).round() as i32;
            let _ = SetWindowPos(d.hwnd, None, 0, -cap, w, h + cap, SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW);
        }
    }
}

/// Visible top-level "DevTools" window of the WebView2 browser process.
fn find_devtools_window(pid: u32, skip: Option<HWND>) -> Option<HWND> {
    struct S {
        pid: u32,
        skip: Option<HWND>,
        found: Option<HWND>,
    }
    extern "system" fn each(hwnd: HWND, l: LPARAM) -> BOOL {
        let s = unsafe { &mut *(l.0 as *mut S) };
        unsafe {
            let mut wpid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut wpid));
            if wpid != s.pid || Some(hwnd) == s.skip || !IsWindowVisible(hwnd).as_bool() {
                return true.into();
            }
            let mut title = [0u16; 64];
            let n = GetWindowTextW(hwnd, &mut title);
            if String::from_utf16_lossy(&title[..n.max(0) as usize]).starts_with("DevTools") {
                s.found = Some(hwnd);
                return false.into();
            }
        }
        true.into()
    }
    let mut s = S { pid, skip, found: None };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut s as *mut S as isize));
    }
    s.found
}

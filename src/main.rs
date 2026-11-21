//! Lightweight browser on WebView2. Usage:
//!   veyra [URL...] [--cycle SECS]
//! URLs open as tabs (the first is activated). `--cycle` activates each tab
//! in turn every SECS seconds (RAM tests).
//!
//! One program, any number of windows: they share the web engine, the ad
//! blocker, history and bookmarks, so a tab moves between windows with its
//! page alive. Starting Veyra again opens a window in the running program.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adblock;
mod anim;
mod app;
mod comp;
mod devtools;
mod downloads;
mod drop;
mod extensions;
mod fullscreen;
mod gfx;
mod multi;
mod newtab;
mod peek;
mod placement;
mod permission;
mod find;
mod history;
mod i18n;
mod settings;
mod shell;
mod shield;
mod shortcuts;
mod store;
mod sys;
mod textfield;
mod ui;
mod update;

use std::{cell::RefCell, path::Path, rc::Rc, sync::mpsc};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::{BOOL, Error, Result};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::*,
            Gdi::ScreenToClient,
        },
        System::{

            LibraryLoader::GetModuleHandleW,
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::{
            Controls::MARGINS,
            HiDpi::*,
            Input::KeyboardAndMouse::{GetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent},
            WindowsAndMessaging::*,
        },
    },
    core::*,
};

use app::App;
use settings::Settings;
use shell::{Mouse, TIMER_ANIM, TIMER_CARET, TIMER_TIP, WM_RENDER};

const TIMER_TICK: usize = 1;
const TIMER_CYCLE: usize = 2;
const TIMER_RETRY: usize = 6;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;

thread_local! {
    /// Every window's state (one `App` per window).
    static APPS: RefCell<Vec<(HWND, Rc<RefCell<App>>)>> = const { RefCell::new(Vec::new()) };
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
    static CYCLE: RefCell<usize> = const { RefCell::new(0) };
    /// Windows tracking the mouse for WM_MOUSELEAVE.
    static TRACKING: RefCell<std::collections::HashSet<isize>> = RefCell::new(std::collections::HashSet::new());
}

/// What all windows share.
struct Shared {
    env: ICoreWebView2Environment,
    blocker: Option<Rc<adblock::Blocker>>,
    store: Rc<RefCell<store::Store>>,
    d3d: (windows::Win32::Graphics::Direct3D11::ID3D11Device, bool),
    data_dir: std::path::PathBuf,
}

/// Runs `f` on window `hwnd`'s app unless it is already borrowed
/// (re-entrant message) or the window is not ours.
pub(crate) fn with_window<R>(hwnd: HWND, f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let app = APPS.with(|a| a.borrow().iter().find(|(h, _)| *h == hwnd).map(|(_, a)| a.clone()))?;
    let mut app = app.try_borrow_mut().ok()?;
    Some(f(&mut app))
}

/// The other windows of this program.
pub(crate) fn other_windows(own: HWND) -> Vec<HWND> {
    APPS.with(|a| a.borrow().iter().map(|(h, _)| *h).filter(|h| *h != own).collect())
}

/// What a new window starts with.
pub(crate) enum Content {
    /// The saved session (first window).
    Restore,
    Urls(Vec<String>),
    /// A live tab moved from another window.
    Tab(app::Tab),
}

pub(crate) struct WindowOpts {
    pub at: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub content: Content,
    /// Held by the pointer (a tab torn off): where the pointer is in it, px.
    pub follow: Option<POINT>,
    /// Private window (app.rs `private`).
    pub private: bool,
}

/// Opens a window. On failure the options come back (with the tab, if one
/// was being moved, so it is not lost).
pub(crate) fn open_window(opts: WindowOpts) -> std::result::Result<HWND, WindowOpts> {
    let Some((env, blocker, store, d3d, data_dir)) = SHARED.with(|s| {
        s.borrow().as_ref().map(|s| (s.env.clone(), s.blocker.clone(), s.store.clone(), s.d3d.clone(), s.data_dir.clone()))
    }) else {
        return Err(opts);
    };
    let first = APPS.with(|a| a.borrow().is_empty());
    let settings = Settings::load(&data_dir);
    let Ok(hwnd) = create_window(opts.at, opts.size, opts.follow.is_none()) else { return Err(opts) };
    let restore = first && matches!(opts.content, Content::Restore);
    // The first window reopens where the last one closed was.
    let maximized = restore && placement::restore(hwnd, &data_dir);
    set_frame(hwnd, settings.title_bar);
    let built = (|| -> Result<App> {
        let comp = comp::Comp::new(hwnd, d3d)?;
        let gfx = gfx::Gfx::new(comp.factory.clone())?;
        let mut ui = ui::Ui::new(gfx, ui::Theme { dark: true, glass: true, private: false })?;
        ui.width = settings.sidebar_width;
        ui.show_nav = settings.show_nav_buttons;
        ui.window_buttons = !settings.title_bar;
        ui.set_mode(&settings.sidebar);
        ui.anims = anim::animations_enabled();
        let scale = unsafe { GetDpiForWindow(hwnd) } as f32 / 96.0;
        Ok(App::new(hwnd, env, settings, data_dir, ui, comp, scale, blocker, store))
    })();
    let Ok(mut app) = built else {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return Err(opts);
    };
    app.private = opts.private;
    app.apply_theme();
    app.refresh_title();
    if first {
        app.migrate_background();
    }
    // A private window starts empty: no download list, no pinned tabs.
    if !app.private {
        app.load_downloads();
        // Pinned tabs are shared by all windows; normal ones come from the session.
        app.load_pins();
    }
    match opts.content {
        Content::Restore => {
            if !app.restore_session() {
                app.run(shortcuts::Action::NewTab);
            }
        }
        Content::Urls(urls) if !urls.is_empty() => {
            let mut first_id = None;
            for url in &urls {
                let url = app.settings.resolve_input(url);
                let id = app.add_tab(url, String::new(), false);
                first_id.get_or_insert(id);
            }
            if let Some(id) = first_id {
                app.activate(id);
            }
        }
        Content::Urls(_) => app.run(shortcuts::Action::NewTab),
        Content::Tab(tab) => {
            let id = app.adopt_tab(tab);
            app.activate(id);
        }
    }
    app.layout();
    APPS.with(|a| a.borrow_mut().push((hwnd, Rc::new(RefCell::new(app)))));
    let target: windows::Win32::System::Ole::IDropTarget = drop::DropTarget(hwnd).into();
    unsafe {
        let _ = windows::Win32::System::Ole::RegisterDragDrop(hwnd, &target);
        SetTimer(Some(hwnd), TIMER_TICK, 5_000, None);
    }
    if let Some(off) = opts.follow {
        shell::begin_follow(hwnd, off);
    }
    unsafe {
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOW });
    }
    Ok(hwnd)
}

fn main() -> Result<()> {
    let mut urls = Vec::new();
    let mut cycle_secs = 0u32;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cycle" => cycle_secs = args.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            _ => urls.push(a),
        }
    }
    // Veyra is already running: it opens the window (one program for all).
    if multi::hand_to_running(&urls) {
        return Ok(());
    }

    unsafe {
        // OLE (not just COM): needed for drag & drop from other apps.
        windows::Win32::System::Ole::OleInitialize(None)?;
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let t0 = std::time::Instant::now();
    // The slow parts of startup run on other threads meanwhile.
    let d3d = std::thread::spawn(comp::Comp::d3d_device);
    let data_dir = sys::data_dir();
    let settings = Settings::load(&data_dir);
    let blocker = settings.adblock.then(|| {
        let dir = data_dir.clone();
        std::thread::spawn(move || adblock::Blocker::read_lists(&dir))
    });
    if !Settings::path(&data_dir).exists() {
        settings.save(&data_dir); // so the user can find and edit every option
    }
    i18n::set(&settings.language);
    let env = create_environment(&data_dir.join("profile"), &browser_args(&settings))?;
    sys::perf("startup: WebView2 environment", t0, 0);
    let blocker = blocker.map(|lists| Rc::new(adblock::Blocker::from_lists(lists.join().expect("adblock lists"))));
    let d3d = d3d.join().expect("d3d thread")?;
    sys::perf("startup: d3d + adblock", t0, 0);
    let store = Rc::new(RefCell::new(store::Store::load(&data_dir)));
    SHARED.with(|s| *s.borrow_mut() = Some(Shared { env, blocker, store, d3d, data_dir }));

    let content = if urls.is_empty() { Content::Restore } else { Content::Urls(urls) };
    let Ok(hwnd) = open_window(WindowOpts { at: None, size: None, content, follow: None, private: false }) else {
        return Err(Error::from(E_POINTER));
    };
    sys::perf("startup: shown", t0, 0);
    unsafe {
        if cycle_secs > 0 {
            SetTimer(Some(hwnd), TIMER_CYCLE, cycle_secs * 1000, None);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// Chromium flags from settings. `BROWSER_DEVTOOLS_PORT` (tests only) opens
/// the remote debugging port used by tools/cdp.mjs.
fn browser_args(s: &Settings) -> String {
    let mut args = vec![s.browser_flags.trim().to_string()];
    if s.low_end_mode {
        args.push("--enable-low-end-device-mode".into());
    }
    if let Ok(port) = std::env::var("BROWSER_DEVTOOLS_PORT") {
        args.push(format!("--remote-debugging-port={port}"));
    }
    args.retain(|a| !a.is_empty());
    args.join(" ")
}

fn create_environment(profile: &Path, args: &str) -> Result<ICoreWebView2Environment> {
    let (tx, rx) = mpsc::channel();
    let profile = HSTRING::from(profile.as_os_str());
    let opts = CoreWebView2EnvironmentOptions::default();
    // Extensions installed from the stores (see extensions.rs).
    unsafe { opts.set_are_browser_extensions_enabled(true) };
    let options: ICoreWebView2EnvironmentOptions = opts.into();
    unsafe { options.SetAdditionalBrowserArguments(&HSTRING::from(args))? };
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &profile, &options, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |err, env| {
            err?;
            tx.send(env.ok_or_else(|| Error::from(E_POINTER))).expect("send env");
            Ok(())
        }),
    )
    .map_err(|e| match e {
        webview2_com::Error::WindowsError(e) => e,
        other => Error::new(E_POINTER, format!("{other:?}")),
    })?;
    rx.recv().expect("env channel")
}

fn reg_dword(name: PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(value)
}

pub(crate) fn system_dark_mode() -> bool {
    reg_dword(w!("AppsUseLightTheme")) == Some(0)
}

/// "Transparency effects" in Windows settings (off: no glass).
pub(crate) fn system_transparency() -> bool {
    reg_dword(w!("EnableTransparency")) != Some(0)
}

/// Dark/light frame and the Acrylic backdrop (glass) or none (solid).
/// Windows' own title bar on top (setting `title_bar`) instead of the
/// window buttons in the sidebar. Read by the window procedure, which may
/// run while the app is busy.
pub(crate) static CLASSIC_FRAME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn classic_frame() -> bool {
    CLASSIC_FRAME.load(std::sync::atomic::Ordering::Relaxed)
}

/// Switches between the frameless window and the classic title bar.
/// Frameless windows have no WS_CAPTION: with it, DWM draws its own
/// minimize/maximize/close buttons wherever our content is transparent.
pub(crate) fn set_frame(hwnd: HWND, classic: bool) {
    CLASSIC_FRAME.store(classic, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        // WS_CAPTION stays in both cases: without it Windows skips the
        // minimize / restore / maximize animations. Frameless, the title bar
        // is hidden by WM_NCCALCSIZE (the client area covers it).
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32 | WS_CAPTION.0;
        let _ = classic;
        SetWindowLongW(hwnd, GWL_STYLE, style as i32);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub(crate) fn apply_backdrop(hwnd: HWND, theme: ui::Theme) {
    let dark = BOOL::from(theme.dark);
    let backdrop = if theme.glass { DWMSBT_TRANSIENTWINDOW } else { DWMSBT_NONE };
    let corners = DWMWCP_ROUND;
    let m = if theme.glass {
        MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 }
    } else {
        MARGINS::default()
    };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, (&dark as *const BOOL).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast(), 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, (&corners as *const DWM_WINDOW_CORNER_PREFERENCE).cast(), 4);
        // Frameless: no caption fill, otherwise DWM paints a darker band
        // where the system title bar would be, on top of the glass.
        let color: u32 = if classic_frame() { 0xFFFF_FFFF } else { 0xFFFF_FFFE }; // DEFAULT / NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, (&color as *const u32).cast(), 4);
    }
}

/// `at`/`size`: a window dragged out of another one (`clamp` keeps it fully
/// on the monitor it was dropped on).
fn create_window(at: Option<(i32, i32)>, size: Option<(i32, i32)>, clamp: bool) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("VeyraMain"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // The app icon embedded by build.rs (resource 1).
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as *const u16)).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let (x, y, w, h) = match at {
            Some((x, y)) if !clamp => {
                let (w, h) = size.unwrap_or((1100, 720));
                (x, y, w, h)
            }
            Some((x, y)) => {
                // Keep a dragged-out window fully on the monitor it was dropped on.
                use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint};
                let (w, h) = size.unwrap_or((1100, 720));
                let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
                let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
                if GetMonitorInfoW(mon, &mut mi).as_bool() {
                    let wa = mi.rcWork;
                    let w = w.min(wa.right - wa.left);
                    let h = h.min(wa.bottom - wa.top);
                    (x.clamp(wa.left, wa.right - w), y.clamp(wa.top, wa.bottom - h), w, h)
                } else {
                    (x, y, w, h)
                }
            }
            None => (CW_USEDEFAULT, CW_USEDEFAULT, 1280, 800),
        };
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("VeyraMain"),
            w!("Veyra"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        Ok(hwnd)
    }
}

/// A torn-off window dropped in place: kept fully on the monitor it was
/// dropped on (as a window opened with `--at`).
pub(crate) fn fit_on_monitor(hwnd: HWND) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    unsafe {
        let mut r = RECT::default();
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetWindowRect(hwnd, &mut r).is_err() || !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return;
        }
        let wa = mi.rcWork;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let (fw, fh) = (w.min(wa.right - wa.left), h.min(wa.bottom - wa.top));
        let (x, y) = (r.left.clamp(wa.left, wa.right - fw), r.top.clamp(wa.top, wa.bottom - fh));
        if (x, y, fw, fh) != (r.left, r.top, w, h) {
            let _ = SetWindowPos(hwnd, None, x, y, fw, fh, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

fn xy(l: LPARAM) -> (i32, i32) {
    ((l.0 & 0xffff) as i16 as i32, ((l.0 >> 16) & 0xffff) as i16 as i32)
}

fn screen_to_client(hwnd: HWND, l: LPARAM) -> (i32, i32) {
    let (x, y) = xy(l);
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    (pt.x, pt.y)
}

fn track(hwnd: HWND, nonclient: bool) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: if nonclient { TME_LEAVE | TME_NONCLIENT } else { TME_LEAVE },
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let mouse = |kind: Mouse, (x, y): (i32, i32), data: u32| {
        if with_window(hwnd, |a| a.mouse(kind, x, y, (w.0 & 0xffff) as u32, data)).is_none() && kind != Mouse::Move {
            // Busy: retry the click/wheel instead of losing it.
            unsafe {
                let _ = PostMessageW(Some(hwnd), msg, w, l);
            }
        }
        LRESULT(0)
    };
    match msg {
        WM_APP => {
            let started = std::time::Instant::now();
            let mut events = app::take_events(hwnd).into_iter();
            while let Some(ev) = events.next() {
                // Busy (re-entrant call): keep the event and the rest for
                // later instead of losing them.
                let mut slot = Some(ev);
                if with_window(hwnd, |a| a.handle(slot.take().expect("event"))).is_none() {
                    let mut rest: Vec<_> = slot.into_iter().collect();
                    rest.extend(events);
                    app::requeue(hwnd, rest);
                    unsafe {
                        SetTimer(Some(hwnd), TIMER_RETRY, 10, None);
                    }
                    break;
                }
            }
            sys::perf("events", started, 15);
            LRESULT(0)
        }
        WM_RENDER => {
            if with_window(hwnd, |a| a.flush_render()).is_none() {
                // App busy (re-entrant call): try again on the next turn.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_RENDER, w, l);
                }
            }
            LRESULT(0)
        }
        // Frameless: the whole window is client area. Maximized windows
        // overhang the screen by the frame size, so trim it.
        WM_NCCALCSIZE if w.0 != 0 && !classic_frame() => unsafe {
            if IsZoomed(hwnd).as_bool() {
                let p = &mut *(l.0 as *mut NCCALCSIZE_PARAMS);
                let dpi = GetDpiForWindow(hwnd);
                let f = GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                p.rgrc[0].left += f;
                p.rgrc[0].top += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].bottom -= f;
            }
            LRESULT(0)
        },
        WM_NCHITTEST => {
            if classic_frame() {
                // Title bar and borders are Windows'; the client area is ours.
                let r = unsafe { DefWindowProcW(hwnd, msg, w, l) };
                if r.0 as u32 != HTCLIENT {
                    return r;
                }
            }
            let (x, y) = screen_to_client(hwnd, l);
            LRESULT(with_window(hwnd, |a| a.nc_hit(x, y)).unwrap_or(HTCLIENT) as isize)
        }
        WM_NCMOUSEMOVE => {
            track(hwnd, true);
            with_window(hwnd, |a| a.nc_hover(w.0 as u32 == HTMAXBUTTON));
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_NCMOUSELEAVE => {
            with_window(hwnd, |a| {
                a.nc_hover(false);
                a.pointer_left();
            });
            LRESULT(0)
        }
        // Our maximize button reports HTMAXBUTTON (snap layouts); handle its
        // click ourselves since there is no system button to press.
        WM_NCLBUTTONDOWN if w.0 as u32 == HTMAXBUTTON => LRESULT(0),
        WM_NCLBUTTONUP if w.0 as u32 == HTMAXBUTTON => {
            with_window(hwnd, |a| a.toggle_maximize());
            LRESULT(0)
        }
        WM_SETCURSOR if (l.0 & 0xffff) as u32 == HTCLIENT => {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
            }
            match with_window(hwnd, |a| a.cursor_at(pt.x, pt.y)).flatten() {
                Some(c) => unsafe {
                    SetCursor(Some(c));
                    LRESULT(1)
                },
                None => unsafe { DefWindowProcW(hwnd, msg, w, l) },
            }
        }
        WM_MOUSEMOVE => {
            if TRACKING.with(|t| t.borrow_mut().insert(hwnd.0 as isize)) {
                track(hwnd, false);
            }
            mouse(Mouse::Move, xy(l), 0)
        }
        WM_MOUSELEAVE => {
            TRACKING.with(|t| t.borrow_mut().remove(&(hwnd.0 as isize)));
            mouse(Mouse::Leave, (-1, -1), 0)
        }
        WM_LBUTTONDOWN => mouse(Mouse::LeftDown, xy(l), 0),
        WM_LBUTTONUP => mouse(Mouse::LeftUp, xy(l), 0),
        WM_LBUTTONDBLCLK => mouse(Mouse::LeftDouble, xy(l), 0),
        WM_RBUTTONDOWN => mouse(Mouse::RightDown, xy(l), 0),
        WM_RBUTTONUP => mouse(Mouse::RightUp, xy(l), 0),
        WM_MBUTTONDOWN => mouse(Mouse::MiddleDown, xy(l), 0),
        WM_MBUTTONUP => mouse(Mouse::MiddleUp, xy(l), 0),
        WM_XBUTTONDOWN => mouse(Mouse::XDown, xy(l), ((w.0 >> 16) & 0xffff) as u32),
        WM_XBUTTONUP => mouse(Mouse::XUp, xy(l), ((w.0 >> 16) & 0xffff) as u32),
        WM_MOUSEWHEEL => mouse(Mouse::Wheel, screen_to_client(hwnd, l), ((w.0 >> 16) & 0xffff) as i16 as i32 as u32),
        WM_MOUSEHWHEEL => mouse(Mouse::HWheel, screen_to_client(hwnd, l), ((w.0 >> 16) & 0xffff) as i16 as i32 as u32),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = w.0 as u16;
            match with_window(hwnd, |a| a.bar_key(vk)) {
                Some(true) => return LRESULT(0),
                None => unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, w, l);
                    return LRESULT(0);
                },
                Some(false) => {}
            }
            match with_window(hwnd, |a| app::window_shortcut(a, vk)).flatten() {
                Some(action) => {
                    with_window(hwnd, |a| a.run(action));
                    LRESULT(0)
                }
                None => unsafe { DefWindowProcW(hwnd, msg, w, l) },
            }
        }
        WM_CHAR => {
            if with_window(hwnd, |a| a.bar_char(w.0 as u16)) == Some(true) {
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_SIZE => {
            let maximized = w.0 as u32 == SIZE_MAXIMIZED;
            let done = with_window(hwnd, |a| {
                a.set_maximized(maximized);
                a.layout();
            });
            if done.is_none() {
                // Sent while the app was busy (e.g. from inside a click
                // handler): lay out again once it is free, or the page keeps
                // its old size.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, w, l);
                }
            }
            LRESULT(0)
        }
        WM_MOVE => {
            with_window(hwnd, |a| a.parent_moved());
            LRESULT(0)
        }
        WM_GETMINMAXINFO => unsafe {
            let info = &mut *(l.0 as *mut MINMAXINFO);
            let s = GetDpiForWindow(hwnd) as i32;
            info.ptMinTrackSize = POINT { x: 640 * s / 96, y: 420 * s / 96 };
            // Without WS_CAPTION Windows would maximize over the taskbar:
            // maximize to the monitor's work area (plus the frame that
            // WM_NCCALCSIZE trims off again).
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !classic_frame() && GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                let dpi = GetDpiForWindow(hwnd);
                let f = GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                let (w, m) = (mi.rcWork, mi.rcMonitor);
                info.ptMaxPosition = POINT { x: w.left - m.left - f, y: w.top - m.top - f };
                info.ptMaxSize = POINT { x: w.right - w.left + 2 * f, y: w.bottom - w.top + 2 * f };
            }
            LRESULT(0)
        },
        WM_DPICHANGED => {
            let dpi = ((w.0 >> 16) & 0xffff) as f32;
            let r = unsafe { *(l.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            with_window(hwnd, |a| a.set_scale(dpi / 96.0));
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            with_window(hwnd, |a| {
                a.ui.anims = anim::animations_enabled();
                a.apply_theme();
                a.layout();
            });
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }
        WM_TIMER => {
            match w.0 {
                TIMER_TICK => {
                    with_window(hwnd, |a| {
                        a.tick();
                        a.download_tick();
                    });
                }
                TIMER_ANIM => {
                    with_window(hwnd, |a| a.frame());
                }
                TIMER_TIP => {
                    with_window(hwnd, |a| a.show_tip());
                }
                TIMER_CARET => {
                    with_window(hwnd, |a| a.blink());
                }
                shell::TIMER_FOLLOW => match shell::follow_step(hwnd) {
                    Some(shell::Follow::Held(pt)) => {
                        with_window(hwnd, |a| a.follow_hover(pt));
                    }
                    Some(shell::Follow::Dropped(pt)) => {
                        with_window(hwnd, |a| a.follow_dropped(pt));
                    }
                    None => {}
                },
                devtools::TIMER_DEVTOOLS => {
                    with_window(hwnd, |a| a.poll_devtools());
                }
                shell::TIMER_NOTICE => {
                    with_window(hwnd, |a| a.clear_notice());
                }
                downloads::TIMER_CARD => {
                    with_window(hwnd, |a| a.card_leave());
                }
                TIMER_RETRY => unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_RETRY);
                    let _ = PostMessageW(Some(hwnd), WM_APP, WPARAM(0), LPARAM(0));
                },
                TIMER_CYCLE => {
                    let n = CYCLE.with(|c| {
                        let mut c = c.borrow_mut();
                        *c += 1;
                        *c
                    });
                    let done = with_window(hwnd, |a| {
                        let i = n % a.tabs.len().max(1);
                        if let Some(id) = a.tabs.get(i).map(|t| t.id) {
                            a.activate(id);
                        }
                        n >= a.tabs.len()
                    });
                    if done == Some(true) {
                        unsafe {
                            let _ = KillTimer(Some(hwnd), TIMER_CYCLE);
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        // A direct download (image) finished: LPARAM is Box<(path, ok, size)>.
        m if m == WM_APP + 5 => {
            let done = unsafe { Box::from_raw(l.0 as *mut (String, bool, i64)) };
            app::post(hwnd, app::Event::DirectDone(done.0, done.1, done.2));
            LRESULT(0)
        }
        // The updater moved on: the settings pages show it; once the
        // installer runs, every window closes (it reopens the new version).
        update::WM_UPDATE => {
            for w in std::iter::once(hwnd).chain(other_windows(hwnd)) {
                with_window(w, |a| a.notify_internal_pages(None));
            }
            if update::status() == update::Status::Installing {
                for w in std::iter::once(hwnd).chain(other_windows(hwnd)) {
                    unsafe {
                        let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            LRESULT(0)
        }
        // The page's cursor changed: shown now if the pointer is on the page
        // (cursor_at decides, as for WM_SETCURSOR; a button held keeps it).
        app::WM_PAGE_CURSOR => {
            unsafe {
                let mut pt = POINT::default();
                if GetCapture().0.is_null()
                    && GetCursorPos(&mut pt).is_ok()
                    && GetAncestor(WindowFromPoint(pt), GA_ROOT) == hwnd
                    && ScreenToClient(hwnd, &mut pt).as_bool()
                    && let Some(c) = with_window(hwnd, |a| a.cursor_at(pt.x, pt.y)).flatten()
                {
                    SetCursor(Some(c));
                }
            }
            LRESULT(0)
        }
        newtab::WM_BG_PICK => {
            if let Some(path) = newtab::pick_file(hwnd) {
                with_window(hwnd, |a| a.bg_picked(path));
            }
            LRESULT(0)
        }
        extensions::WM_EXT_PICK => {
            if let Some(path) = extensions::pick_folder(hwnd) {
                with_window(hwnd, |a| a.load_unpacked(path));
            }
            LRESULT(0)
        }
        extensions::WM_EXT_UNPACKED => {
            // Owned box from the download thread; retried while busy.
            let raw = l.0 as *mut extensions::Unpacked;
            let mut slot = Some(unsafe { Box::from_raw(raw) });
            if with_window(hwnd, |a| a.extension_unpacked(*slot.take().expect("unpacked"))).is_none() {
                // Busy: `slot` was consumed only if the closure ran.
            }
            if let Some(b) = slot {
                let raw = Box::into_raw(b);
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, w, LPARAM(raw as isize));
                }
            }
            LRESULT(0)
        }
        devtools::WM_DOCK_GONE => {
            let child = HWND(l.0 as *mut _);
            with_window(hwnd, |a| a.child_destroyed(child));
            LRESULT(0)
        }
        // A child went away: the docked DevTools was closed.
        WM_PARENTNOTIFY if (w.0 & 0xffff) as u32 == WM_DESTROY => {
            let child = HWND(l.0 as *mut _);
            with_window(hwnd, |a| a.child_destroyed(child));
            LRESULT(0)
        }
        // Messages from the other Veyra windows (multi.rs).
        WM_COPYDATA => {
            let data = unsafe { &*(l.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT) };
            if data.cbData > 256 * 1024 || (data.lpData.is_null() && data.cbData > 0) {
                return LRESULT(0);
            }
            let json = if data.cbData == 0 {
                String::new()
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
                String::from_utf8_lossy(bytes).into_owned()
            };
            let reply = |r: isize| unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::ReplyMessage(LRESULT(r));
            };
            match data.dwData {
                // Veyra started again: a new window here (multi::hand_to_running).
                multi::OPEN_WINDOW => {
                    reply(1);
                    let urls = serde_json::from_str::<Vec<String>>(&json).unwrap_or_default();
                    if let Ok(w) = open_window(WindowOpts { at: None, size: None, content: Content::Urls(urls), follow: None, private: false }) {
                        unsafe {
                            let _ = SetForegroundWindow(w);
                        }
                    }
                    LRESULT(1)
                }
                multi::TAB_HOVER => LRESULT(with_window(hwnd, |a| a.tab_hover(&json)).unwrap_or(false) as isize),
                multi::TAB_HOVER_END => {
                    reply(1);
                    with_window(hwnd, |a| a.end_incoming());
                    LRESULT(1)
                }
                multi::CLOSED_TABS => {
                    reply(1);
                    with_window(hwnd, |a| a.take_closed(&json));
                    LRESULT(1)
                }
                _ => LRESULT(0),
            }
        }
        // Test scripts: how many tabs this window has (normal | pinned << 16).
        history::WM_TEST_HISTORY => LRESULT(
            with_window(hwnd, |a| {
                a.toggle_history();
                a.history_open() as isize
            })
            .unwrap_or(-1),
        ),
        multi::WM_TAB_COUNT => LRESULT(
            with_window(hwnd, |a| {
                let pinned = a.tabs.iter().filter(|t| t.pinned).count();
                (a.tabs.len() - pinned) as isize | (pinned as isize) << 16
            })
            .unwrap_or(-1),
        ),
        multi::WM_PINS => {
            if with_window(hwnd, |a| a.reload_pins()).is_none() {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, w, l); // busy: on the next turn
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_window(hwnd, |a| a.shutdown());
            let last = APPS.with(|a| {
                let mut a = a.borrow_mut();
                a.retain(|(h, _)| *h != hwnd);
                a.is_empty()
            });
            TRACKING.with(|t| t.borrow_mut().remove(&(hwnd.0 as isize)));
            unsafe {
                let _ = windows::Win32::System::Ole::RevokeDragDrop(hwnd);
                if last {
                    PostQuitMessage(0);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

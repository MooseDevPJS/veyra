//! Browser state: tabs and their WebViews, the RAM policy (live → suspended
//! → unloaded), shortcuts and session restore.
//!
//! WebView2 callbacks never touch `App` directly: they push an [`Event`]
//! and post `WM_APP`, and the window procedure drains the queue. This keeps
//! the single `RefCell<App>` borrow from ever nesting.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::core::Result;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::{DirectComposition::IDCompositionVisual2, Imaging::IWICBitmap},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        },
        UI::WindowsAndMessaging::{HCURSOR, PostMessageW, WM_APP},
    },
    core::*,
};

use crate::{
    adblock::Blocker,
    comp::Comp,
    settings::Settings,
    shield::{self, ShieldState},
    shortcuts::{Action, Keymap},
    store::{self, Store},
    sys,
    ui::{TabView, Ui},
};

pub type TabId = u64;

thread_local! {
    /// Cursor the active page of each window wants (from CursorChanged).
    pub static PAGE_CURSOR: RefCell<HashMap<isize, HCURSOR>> = RefCell::new(HashMap::new());
    /// Which window each tab is in now (tabs move between windows alive).
    static TAB_OWNER: RefCell<HashMap<TabId, HWND>> = RefCell::new(HashMap::new());
    /// Tab ids are unique in the whole program (all windows).
    static NEXT_TAB_ID: Cell<TabId> = const { Cell::new(1) };
    /// Ctrl+T/Ctrl+L was pressed and the command bar is about to open:
    /// characters typed meanwhile are kept here instead of being lost.
    pub static BAR_PENDING: Cell<bool> = const { Cell::new(false) };
    pub static TYPED_AHEAD: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
    /// Enter was pressed before the bar finished opening.
    pub static ENTER_AHEAD: Cell<bool> = const { Cell::new(false) };
    /// Last middle click / Ctrl+click in a page: a link it opens becomes a
    /// background tab.
    pub static BACKGROUND_CLICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub enum Event {
    Created(TabId, ICoreWebView2CompositionController),
    CreateFailed(TabId),
    Title(TabId, String),
    Url(TabId, String),
    Audio(TabId, bool),
    Shortcut(Action),
    /// Popup / target=_blank. Completing the deferral after the new tab's
    /// WebView exists keeps `window.opener` (needed by OAuth sign-in popups).
    NewWindow(String, ICoreWebView2NewWindowRequestedEventArgs, ICoreWebView2Deferral),
    Crashed(TabId),
    /// PNG bytes of the page's favicon.
    Favicon(TabId, Vec<u8>),
    /// JSON message from the internal settings page.
    SettingsPage(TabId, String),
    NavStart(TabId),
    NavDone(TabId),
    /// Right-click inside a page: our own menu replaces the default one.
    PageMenu(TabId, ICoreWebView2ContextMenuRequestedEventArgs, ICoreWebView2Deferral),
    /// Alt+click on a link: preview it.
    Peek(String),
    PeekClose,
    Permission(TabId, ICoreWebView2PermissionRequestedEventArgs, ICoreWebView2Deferral),
    /// A store page asked to install an extension: (id, from Edge Add-ons).
    InstallExtension(String, bool),
    Notice(String),
    OpenUrl(String),
    Extensions(Vec<crate::extensions::Ext>),
    /// Installed, removed: reload the list.
    ExtensionsChanged,
    /// A page entered (true) or left fullscreen.
    FullScreen(TabId, bool),
    DownloadStarted(TabId, ICoreWebView2DownloadOperation, String),
    DownloadProgress(ICoreWebView2DownloadOperation),
    /// Image under the pointer at the last right-click ("" = none).
    ContextImage(String),
    /// A direct download (image) ended: (index-independent path, ok).
    DirectDone(String, bool, i64),
    /// A page failed to load: (tab, failed address, error kind).
    LoadFailed(TabId, String, &'static str),
}

/// Internal pages are served from the `ui` folder under this host.
pub const INTERNAL_HOST: &str = "browser.settings";
pub const SETTINGS_URL: &str = "https://browser.settings/settings.html";
pub const NEWTAB_URL: &str = "https://browser.settings/newtab.html";

fn ui_dir() -> PathBuf {
    let next_to_exe = sys::exe_dir().join("ui");
    if next_to_exe.exists() { next_to_exe } else { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui") }
}

thread_local! {
    static EVENTS: RefCell<VecDeque<(Target, Event)>> = const { RefCell::new(VecDeque::new()) };
}

/// Where an event goes: a window, or whatever window a tab is in when the
/// event is handled (its WebView's callbacks use this, see `Owner`).
#[derive(Clone, Copy)]
pub(crate) enum Target {
    Window(HWND),
    Tab(TabId),
}

/// A tab's WebView callbacks post through this: it follows the tab when
/// the tab moves to another window.
#[derive(Clone, Copy)]
pub(crate) struct Owner(pub TabId);

pub(crate) trait PostTarget: Copy {
    fn target(self) -> Target;
}
impl PostTarget for HWND {
    fn target(self) -> Target {
        Target::Window(self)
    }
}
impl PostTarget for Owner {
    fn target(self) -> Target {
        Target::Tab(self.0)
    }
}

impl Target {
    fn window(self) -> Option<HWND> {
        match self {
            Target::Window(h) => Some(h),
            Target::Tab(id) => TAB_OWNER.with(|m| m.borrow().get(&id).copied()),
        }
    }
}

pub(crate) fn tab_owner(id: TabId) -> Option<HWND> {
    Target::Tab(id).window()
}

fn set_tab_owner(id: TabId, hwnd: Option<HWND>) {
    TAB_OWNER.with(|m| match hwnd {
        Some(h) => {
            m.borrow_mut().insert(id, h);
        }
        None => {
            m.borrow_mut().remove(&id);
        }
    });
}

/// The page changed its cursor (a link hand, a video hiding it...). Shown at
/// once when the pointer is over our window: waiting for the next
/// WM_SETCURSOR left it hidden after a video brought it back.
pub(crate) fn page_cursor_changed(to: impl PostTarget, h: HCURSOR) {
    let Some(hwnd) = to.target().window() else { return };
    PAGE_CURSOR.with(|p| p.borrow_mut().insert(hwnd.0 as isize, h));
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt).is_ok()
            && windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(pt),
                windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
            ) == hwnd
            && windows::Win32::UI::Input::KeyboardAndMouse::GetCapture().0.is_null()
        {
            windows::Win32::UI::WindowsAndMessaging::SetCursor(Some(h));
        }
    }
}

/// Our error page stands for the address that failed to load.
fn error_page_target(url: &str) -> Option<String> {
    let rest = url.strip_prefix(&format!("https://{INTERNAL_HOST}/error.html?"))?;
    let u = rest.split('&').find_map(|kv| kv.strip_prefix("u="))?;
    Some(crate::settings::url_decode(u))
}

pub(crate) fn post(to: impl PostTarget, ev: Event) {
    let target = to.target();
    let Some(hwnd) = target.window() else { return }; // its tab is gone
    EVENTS.with(|q| q.borrow_mut().push_back((target, ev)));
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP, WPARAM(0), LPARAM(0));
    }
}

/// The queued events for window `hwnd` (a tab's go to the window it is in
/// now). Events of tabs that no longer exist are dropped.
pub fn take_events(hwnd: HWND) -> Vec<Event> {
    EVENTS.with(|q| {
        let mut q = q.borrow_mut();
        let mut mine = Vec::new();
        let mut rest = VecDeque::new();
        for (t, ev) in q.drain(..) {
            match t.window() {
                Some(h) if h == hwnd => mine.push(ev),
                Some(_) => rest.push_back((t, ev)),
                None => {}
            }
        }
        *q = rest;
        mine
    })
}

/// Puts events back at the front of the queue (the app was busy).
pub fn requeue(hwnd: HWND, events: Vec<Event>) {
    EVENTS.with(|q| {
        let mut q = q.borrow_mut();
        for ev in events.into_iter().rev() {
            q.push_front((Target::Window(hwnd), ev));
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabState {
    /// WebView being created.
    Loading,
    Live,
    /// Kept in RAM, no CPU.
    Suspended,
    /// Only URL and title kept; WebView destroyed.
    Unloaded,
}

pub(crate) struct View {
    pub controller: ICoreWebView2Controller,
    /// Visual hosting: mouse input is forwarded through this.
    pub comp: ICoreWebView2CompositionController,
    pub webview: ICoreWebView2,
    /// Container visual the page renders into (shown when active).
    pub visual: IDCompositionVisual2,
}

pub struct Tab {
    pub id: TabId,
    pub url: String,
    pub title: String,
    pub pinned: bool,
    /// Pinned: its id in `pins.json`, the same in every window (multi.rs).
    pub pin: u64,
    pub state: TabState,
    pub playing_audio: bool,
    last_active: Instant,
    pub(crate) view: Option<View>,
    shield: Rc<ShieldState>,
    pending_popup: Option<(ICoreWebView2NewWindowRequestedEventArgs, ICoreWebView2Deferral)>,
    /// Group this tab belongs to (see `Group`).
    pub group: Option<u32>,
    /// Start of the current WebView creation / navigation (perf tracing).
    timing: Option<Instant>,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    /// Normal tabs (the pinned ones are in `pins.json`, see multi.rs).
    tabs: Vec<SavedTab>,
    active: usize,
    /// The active tab was a pinned one.
    #[serde(default)]
    active_pin: Option<u64>,
    #[serde(default)]
    groups: Vec<Group>,
}

/// A named, collapsible set of tabs in the list (like a folder).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Group {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct SavedTab {
    url: String,
    title: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    group: Option<u32>,
}

pub struct App {
    pub(crate) hwnd: HWND,
    pub(crate) env: ICoreWebView2Environment,
    pub(crate) blocker: Option<Rc<Blocker>>,
    pub(crate) keymap: Rc<RefCell<Keymap>>,
    pub settings: Settings,
    pub(crate) data_dir: PathBuf,
    pub tabs: Vec<Tab>,
    pub(crate) active: Option<TabId>,
    closed: Vec<SavedTab>,
    /// Favicons by host, color + grayscale; also cached on disk.
    pub icons: HashMap<String, (IWICBitmap, IWICBitmap)>,
    pub(crate) icon_misses: HashSet<String>,
    pub(crate) session_dirty: bool,
    /// History and bookmarks, shared by all windows.
    pub store: Rc<RefCell<Store>>,
    /// A setting changed that only applies after restarting.
    restart_pending: bool,
    pub ui: Ui,
    pub comp: Comp,
    /// DPI scale (1.0 = 96 dpi).
    pub scale: f32,
    /// Pinned tab ids as the other windows know them (multi.rs).
    pub(crate) pins_sent: Vec<u64>,
    /// Pinned tabs came from `pins.json` (not from the session).
    pub(crate) pins_loaded: bool,
    /// Preview row of a tab another window is bringing (multi.rs, `tab_hover`).
    pub(crate) incoming: Option<TabId>,
    pub(crate) incoming_at: Instant,
    /// Torn off and following the pointer: the window under it, whether
    /// this one is hidden over its sidebar, the last position handled.
    pub(crate) follow_target: Option<HWND>,
    pub(crate) follow_hidden: bool,
    pub(crate) follow_last: Option<(i32, i32)>,
    /// Dragging the sidebar edge.
    pub(crate) resizing: bool,
    /// A mouse button went down in the page: keep sending it events.
    pub(crate) page_capture: bool,
    pub(crate) mouse_in_page: bool,
    /// What the frame layer currently holds (solid?, w, h, dark).
    pub(crate) frame_drawn: (bool, u32, u32, bool),
    pub(crate) groups: Vec<Group>,
    /// A repaint is queued (see `redraw`).
    pub(crate) render_pending: bool,
    /// DevTools docked as a pane (see devtools.rs).
    pub(crate) dock: Option<crate::devtools::Dock>,
    pub(crate) dock_wait: Option<crate::devtools::Pending>,
    pub(crate) resizing_dock: bool,
    pub(crate) dock_close_hover: bool,
    /// Drag from another app currently over the window (drop.rs).
    pub(crate) drag_over: crate::drop::Over,
    pub(crate) drag_data: Option<windows::Win32::System::Com::IDataObject>,
    /// Alt+click link preview.
    pub(crate) peek: Option<crate::peek::Peek>,
    /// Site permission requests waiting for an answer.
    pub(crate) prompts: Vec<crate::permission::Prompt>,
    /// Short message shown over the page (extensions install result...).
    pub(crate) notice: Option<String>,
    /// Installed extensions (for the extensions button) and their on/off policy.
    pub(crate) exts: Vec<crate::extensions::Ext>,
    pub(crate) exts_loaded: bool,
    pub(crate) ext_policy: crate::extensions::Policy,
    /// Page fullscreen: the window placement to go back to.
    pub(crate) fullscreen: Option<windows::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT>,
    /// Downloads (newest first), the card over the page and when the last one ran.
    pub(crate) downloads: Vec<crate::downloads::Record>,
    pub(crate) card: Option<crate::downloads::Card>,
    pub(crate) last_download: Option<Instant>,
    /// 1 ms timer resolution requested (only while animating).
    pub(crate) fine_timer: bool,
    /// The open menu is the downloads list (kept live while it is open).
    pub(crate) downloads_menu: bool,
    /// Image address from the page's last right-click (see peek::page_script).
    pub(crate) context_image: String,
    /// (tab they were opened from, last one opened) for background tabs, so
    /// several of them line up in the order they were clicked.
    pub(crate) background_run: Option<(TabId, TabId)>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hwnd: HWND,
        env: ICoreWebView2Environment,
        settings: Settings,
        data_dir: PathBuf,
        ui: Ui,
        comp: Comp,
        scale: f32,
        blocker: Option<Rc<Blocker>>,
        store: Rc<RefCell<Store>>,
    ) -> Self {
        let keymap = Rc::new(RefCell::new(Keymap::new(&settings.shortcuts)));
        let _ = std::fs::create_dir_all(data_dir.join("favicons"));
        let ext_policy = crate::extensions::Policy::load(&data_dir);
        App {
            store,
            restart_pending: false,
            hwnd,
            env,
            blocker,
            keymap,
            settings,
            data_dir,
            tabs: Vec::new(),
            active: None,
            closed: Vec::new(),
            icons: HashMap::new(),
            icon_misses: HashSet::new(),
            session_dirty: false,
            ui,
            comp,
            scale,
            pins_sent: Vec::new(),
            pins_loaded: false,
            incoming: None,
            incoming_at: Instant::now(),
            follow_target: None,
            follow_hidden: false,
            follow_last: None,
            resizing: false,
            page_capture: false,
            mouse_in_page: false,
            frame_drawn: (false, 0, 0, false),
            groups: Vec::new(),
            render_pending: false,
            dock: None,
            dock_wait: None,
            resizing_dock: false,
            dock_close_hover: false,
            drag_over: crate::drop::Over::Nothing,
            drag_data: None,
            peek: None,
            prompts: Vec::new(),
            notice: None,
            exts: Vec::new(),
            exts_loaded: false,
            ext_policy,
            fullscreen: None,
            downloads: Vec::new(),
            card: None,
            last_download: None,
            fine_timer: false,
            downloads_menu: false,
            context_image: String::new(),
            background_run: None,
        }
    }

    // ---------- tabs ----------

    fn index(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub(crate) fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.index(id))
    }

    /// Adds a tab after the active one (unloaded until activated).
    pub fn add_tab(&mut self, url: String, title: String, pinned: bool) -> TabId {
        let id = NEXT_TAB_ID.with(|n| n.replace(n.get() + 1));
        set_tab_owner(id, Some(self.hwnd));
        let tab = Tab {
            id,
            url,
            title,
            pinned,
            pin: 0,
            state: TabState::Unloaded,
            playing_audio: false,
            last_active: Instant::now(),
            view: None,
            shield: Rc::new(ShieldState::default()),
            pending_popup: None,
            group: None,
            timing: None,
        };
        let at = self.active_index().map_or(self.tabs.len(), |i| i + 1);
        // Opened from a grouped tab: it joins the same group (as in Chrome).
        let group = self.active_index().filter(|_| !pinned).and_then(|i| self.tabs[i].group);
        let mut tab = tab;
        tab.group = group;
        self.tabs.insert(at, tab);
        self.session_dirty = true;
        id
    }

    pub fn open(&mut self, url: String) {
        let id = self.add_tab(url, String::new(), false);
        self.activate(id);
    }

    pub fn activate(&mut self, id: TabId) {
        if self.active == Some(id) {
            return;
        }
        // Fullscreen belongs to the page that asked for it.
        self.leave_fullscreen();
        if let Some(prev) = self.active.and_then(|p| self.tab_mut(p)) {
            prev.last_active = Instant::now();
            if let Some(v) = &prev.view {
                unsafe {
                    let _ = v.controller.SetIsVisible(false);
                }
                set_memory_level(&v.webview, true);
            }
        }
        self.active = Some(id);
        self.ui.reveal = Some(id); // scrolled into view at the next render
        PAGE_CURSOR.with(|p| p.borrow_mut().remove(&(self.hwnd.0 as isize))); // the new page reports its own
        self.apply_extensions();
        let bounds = self.page_bounds();
        let bar_open = self.ui.bar.open;
        let Some(tab) = self.tab_mut(id) else { return };
        tab.last_active = Instant::now();
        let mut shown = None;
        match &tab.view {
            None if tab.state != TabState::Loading => self.create_view(id),
            None => {}
            Some(v) => unsafe {
                set_memory_level(&v.webview, false);
                let _ = v.controller.SetBounds(bounds);
                let _ = v.controller.SetIsVisible(true); // also resumes a suspended tab
                if !bar_open {
                    let _ = v.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
                tab.state = TabState::Live;
                shown = Some(v.visual.clone());
            },
        }
        self.comp.show_page(shown.as_ref());
        self.session_dirty = true;
        self.refresh_title();
    }

    pub fn close(&mut self, id: TabId) {
        let Some(i) = self.index(id) else { return };
        self.drop_prompts(id);
        let tab = self.tabs.remove(i);
        set_tab_owner(id, None);
        if !tab.url.is_empty() {
            self.closed.push(SavedTab { url: tab.url.clone(), title: tab.title.clone(), pinned: tab.pinned, group: None });
        }
        if let Some(v) = tab.view {
            unsafe {
                let _ = v.controller.Close();
            }
        }
        if self.active == Some(id) {
            self.active = None;
            self.comp.show_page(None);
            if let Some(next) = self.tabs.get(i.min(self.tabs.len().saturating_sub(1))) {
                let next = next.id;
                self.activate(next);
            }
        }
        self.session_dirty = true;
        self.refresh_title();
    }

    /// Takes a tab out of this window WITHOUT closing its page (it moves to
    /// another window alive: see `adopt_tab`).
    pub(crate) fn take_tab(&mut self, id: TabId) -> Option<Tab> {
        let i = self.index(id)?;
        if self.active == Some(id) {
            self.leave_fullscreen();
        }
        if self.dock.as_ref().is_some_and(|d| d.tab == id) {
            self.close_devtools();
        }
        self.drop_prompts(id);
        let tab = self.tabs.remove(i);
        if let Some(v) = &tab.view {
            unsafe {
                let _ = v.controller.SetIsVisible(false);
            }
        }
        if self.active == Some(id) {
            self.active = None;
            self.comp.show_page(None);
            if let Some(next) = self.tabs.get(i.min(self.tabs.len().saturating_sub(1))) {
                let next = next.id;
                self.activate(next);
            }
        }
        self.session_dirty = true;
        self.refresh_title();
        self.redraw();
        Some(tab)
    }

    /// A live tab from another window joins this one (after the active tab;
    /// the caller activates it). Its page is shown here from now on.
    pub(crate) fn adopt_tab(&mut self, mut tab: Tab) -> TabId {
        let id = tab.id;
        set_tab_owner(id, Some(self.hwnd));
        tab.group = None; // groups belong to their window
        if let Some(v) = &mut tab.view {
            unsafe {
                let _ = v.controller.SetParentWindow(self.hwnd);
                if let Ok(visual) = self.comp.tab_visual()
                    && v.comp.SetRootVisualTarget(&visual).is_ok()
                {
                    v.visual = visual;
                }
            }
        }
        let at = self.active_index().map_or(self.tabs.len(), |i| i + 1);
        self.tabs.insert(at.min(self.tabs.len()), tab);
        self.session_dirty = true;
        // Events its page queued meanwhile are handled here now.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP, WPARAM(0), LPARAM(0));
        }
        id
    }

    pub(crate) fn create_view(&mut self, id: TabId) {
        let (hwnd, owner) = (self.hwnd, Owner(id));
        if let Some(tab) = self.tab_mut(id) {
            tab.state = TabState::Loading;
            tab.timing = Some(Instant::now());
        }
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(move |err, controller| {
            match (err, controller) {
                (Ok(()), Some(c)) => post(owner, Event::Created(id, c)),
                (e, _) => {
                    eprintln!("tab {id}: WebView creation failed: {e:?}");
                    post(owner, Event::CreateFailed(id));
                }
            }
            Ok(())
        }));
        unsafe {
            let created = self
                .env
                .cast::<ICoreWebView2Environment3>()
                .and_then(|env3| env3.CreateCoreWebView2CompositionController(hwnd, &handler));
            if let Err(e) = created {
                eprintln!("tab {id}: CreateCoreWebView2CompositionController: {e}");
                if let Some(tab) = self.tab_mut(id) {
                    tab.state = TabState::Unloaded;
                }
            }
        }
    }

    fn on_created(&mut self, id: TabId, cc: ICoreWebView2CompositionController) {
        let bounds = self.page_bounds();
        let is_active = self.active == Some(id);
        // Callbacks reach the tab's window through its owner (it can move).
        let (env, blocker, keymap, hwnd) =
            (self.env.clone(), self.blocker.clone(), self.keymap.clone(), Owner(id));
        let prefs = self.settings.clone();
        let data_dir = self.data_dir.clone();
        let Ok(controller) = cc.cast::<ICoreWebView2Controller>() else { return };
        let visual = self.comp.tab_visual();
        let bar_open = self.ui.bar.open;
        let dark = self.ui.theme.dark;
        let Some(tab) = self.tab_mut(id) else {
            // Tab closed while its WebView was being created.
            unsafe {
                let _ = controller.Close();
            }
            return;
        };
        let result: Result<View> = (|| unsafe {
            let visual = visual?;
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
            // Theme color instead of a white flash while the page loads.
            if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
                let bg = if dark { 0x16u8 } else { 0xF4u8 };
                let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: bg, G: bg, B: bg + 2 });
            }
            let webview = controller.CoreWebView2()?;
            let settings = webview.Settings()?;
            settings.SetIsStatusBarEnabled(false)?; // no URL bubble bottom-left
            settings.SetIsBuiltInErrorPageEnabled(false)?; // ours, not Edge's
            apply_privacy(&webview, &settings, &prefs);
            if let Ok(wv3) = webview.cast::<ICoreWebView2_3>() {
                wv3.SetVirtualHostNameToFolderMapping(
                    &HSTRING::from(INTERNAL_HOST),
                    &HSTRING::from(ui_dir().as_os_str()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
                )?;
            }
            if let Some(b) = blocker {
                shield::install(&env, &webview, b, tab.shield.clone())?;
            }
            crate::newtab::install(&webview, &data_dir)?;
            wire_events(id, hwnd, &controller, &webview, keymap)?;
            for script in [crate::peek::page_script(shield::token()), crate::extensions::store_script(shield::token())] {
                webview.AddScriptToExecuteOnDocumentCreated(
                    &HSTRING::from(script),
                    &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
                )?;
            }
            let mut peek_token = 0;
            webview.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut p = PWSTR::null();
                    args.WebMessageAsJson(&mut p)?;
                    let json = CoTaskMemPWSTR::from(p).to_string();
                    let Ok(msg) = serde_json::from_str::<serde_json::Value>(&json) else { return Ok(()) };
                    if msg.get("k").and_then(|k| k.as_str()) != Some(shield::token()) {
                        return Ok(());
                    }
                    if let Some(url) = msg.get("peek").and_then(|u| u.as_str())
                        && url.starts_with("http")
                    {
                        post(hwnd, Event::Peek(url.to_string()));
                    }
                    if let Some(src) = msg.get("ctximg").and_then(|u| u.as_str()) {
                        post(hwnd, Event::ContextImage(src.to_string()));
                    }
                    // Installs only when the message really comes from a store page.
                    if let Some(ext) = msg.get("install").and_then(|u| u.as_str()) {
                        let mut p = PWSTR::null();
                        args.Source(&mut p)?;
                        let source = host_of(&CoTaskMemPWSTR::from(p).to_string());
                        let edge = source == "microsoftedge.microsoft.com";
                        if edge || source == "chromewebstore.google.com" || source == "chrome.google.com" {
                            post(hwnd, Event::InstallExtension(ext.to_string(), edge));
                        }
                    }
                    Ok(())
                })),
                &mut peek_token,
            )?;
            controller.SetBounds(bounds)?;
            controller.SetIsVisible(is_active)?;
            if let Some((args, deferral)) = tab.pending_popup.take() {
                args.SetNewWindow(&webview)?;
                deferral.Complete()?;
            } else if !tab.url.is_empty() {
                webview.Navigate(&HSTRING::from(tab.url.as_str()))?;
            }
            if is_active && !bar_open {
                controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)?;
            } else if !is_active {
                set_memory_level(&webview, true);
            }
            Ok(View { controller: controller.clone(), comp: cc.clone(), webview, visual })
        })();
        match result {
            Ok(view) => {
                let shown = is_active.then(|| view.visual.clone());
                tab.view = Some(view);
                tab.state = TabState::Live;
                if let Some(v) = shown {
                    self.comp.show_page(Some(&v));
                    self.layout();
                }
            }
            Err(e) => {
                eprintln!("tab {id}: setup failed: {e}");
                unsafe {
                    let _ = controller.Close();
                }
                tab.state = TabState::Unloaded;
            }
        }
    }

    fn unload(&mut self, id: TabId) {
        if let Some(tab) = self.tab_mut(id)
            && let Some(v) = tab.view.take()
        {
            unsafe {
                let _ = v.controller.Close();
            }
            tab.state = TabState::Unloaded;
            tab.playing_audio = false;
            eprintln!("tab {id}: unloaded ({})", tab.url);
        }
    }

    fn suspend(&mut self, id: TabId) {
        let Some(tab) = self.tab_mut(id) else { return };
        let Some(v) = &tab.view else { return };
        if let Ok(wv3) = v.webview.cast::<ICoreWebView2_3>() {
            let handler = TrySuspendCompletedHandler::create(Box::new(|_, _| Ok(())));
            unsafe {
                let _ = wv3.TrySuspend(&handler);
            }
            tab.state = TabState::Suspended;
        }
    }

    /// Runs every few seconds: suspends/unloads background tabs per settings,
    /// and unloads early when the system is short on RAM.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let s = &self.settings;
        let suspend_after = Duration::from_secs(s.suspend_after_secs);
        let unload_after = Duration::from_secs(s.unload_after_secs);
        let mut to_suspend = Vec::new();
        let mut to_unload = Vec::new();
        for t in &self.tabs {
            if Some(t.id) == self.active || t.view.is_none() || (t.playing_audio && s.keep_audio_tabs) {
                continue;
            }
            let idle = now - t.last_active;
            if s.unload_after_secs > 0 && idle >= unload_after {
                to_unload.push(t.id);
            } else if s.suspend_after_secs > 0 && idle >= suspend_after && t.state == TabState::Live {
                to_suspend.push(t.id);
            }
        }
        if s.low_memory_mb > 0 && sys::free_ram_mb() < s.low_memory_mb {
            let oldest = self
                .tabs
                .iter()
                .filter(|t| Some(t.id) != self.active && t.view.is_some() && !(t.playing_audio && s.keep_audio_tabs))
                .min_by_key(|t| t.last_active)
                .map(|t| t.id);
            to_unload.extend(oldest);
        }
        for id in to_suspend {
            self.suspend(id);
        }
        for id in to_unload {
            self.unload(id);
        }
        if self.session_dirty {
            self.save_session();
        }
        self.store.borrow_mut().save();
        self.check_dock();
        self.expire_incoming();
    }

    /// A tab closed in another window (multi.rs): Ctrl+Shift+T can bring it here.
    pub(crate) fn push_closed(&mut self, url: String, title: String) {
        self.closed.push(SavedTab { url, title, pinned: false, group: None });
    }

    // ---------- events ----------

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Created(crate::peek::PEEK_ID, c) => self.peek_created(c),
            Event::Peek(url) => self.open_peek(url),
            Event::PeekClose => self.close_peek(),
            Event::Permission(id, args, deferral) => self.permission_requested(id, args, deferral),
            Event::InstallExtension(id, edge) => self.install_extension(id, edge),
            Event::Notice(text) => self.notify(&text),
            Event::Extensions(list) => self.extensions_loaded(list),
            Event::ExtensionsChanged => self.refresh_extensions(),
            Event::FullScreen(id, on) => self.page_fullscreen(id, on),
            Event::DownloadStarted(id, op, path) => self.download_started(id, op, path),
            Event::DownloadProgress(op) => self.download_progress(op),
            Event::ContextImage(src) => self.context_image = src,
            Event::DirectDone(path, ok, size) => self.direct_done(&path, ok, size),
            Event::LoadFailed(id, url, kind) => {
                // Our own error page (never Edge's), kept at the page's address.
                if let Some(v) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.view.as_ref()) {
                    let page = format!("https://{INTERNAL_HOST}/error.html?e={kind}&l={}&u={}", crate::i18n::code(), crate::settings::url_encode(&url));
                    unsafe {
                        let _ = v.webview.Navigate(&HSTRING::from(page));
                    }
                }
            }
            Event::OpenUrl(url) => {
                let id = self.add_tab(url, String::new(), false);
                self.activate(id);
            }
            Event::Created(id, c) => {
                if let Some(t) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.timing) {
                    sys::perf(&format!("tab {id} webview created"), t, 0);
                }
                let s = Instant::now();
                self.on_created(id, c);
                sys::perf(&format!("tab {id} on_created"), s, 0);
            }
            Event::NavStart(id) => {
                if let Some(t) = self.tab_mut(id) {
                    t.timing = Some(Instant::now());
                }
            }
            Event::NavDone(id) => {
                if let Some(t) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.timing) {
                    sys::perf(&format!("tab {id} navigation done"), t, 0);
                }
            }
            Event::CreateFailed(id) => {
                if let Some(t) = self.tab_mut(id) {
                    t.state = TabState::Unloaded;
                }
            }
            Event::Title(id, title) => {
                if let Some(t) = self.tab_mut(id) {
                    t.title = title;
                }
                if let Some(t) = self.tabs.iter().find(|t| t.id == id) {
                    self.store.borrow_mut().set_title(&t.url, &t.title);
                }
                self.session_dirty = true;
                self.refresh_title();
            }
            Event::Url(id, url) => {
                // Chromium turns the pages of a removed extension into its
                // own new tab page, which does not exist here: close them.
                if url.starts_with("chrome-search://") {
                    return self.close(id);
                }
                let url = error_page_target(&url).unwrap_or(url);
                if !url.starts_with(&format!("https://{INTERNAL_HOST}/")) {
                    self.store.borrow_mut().record_visit(&url, store::now());
                }
                if let Some(t) = self.tab_mut(id) {
                    t.url = url;
                }
                if self.active == Some(id) {
                    self.apply_extensions();
                }
                self.session_dirty = true;
                self.redraw();
            }
            Event::Favicon(id, png) => {
                let Some(host) = self.tabs.iter().find(|t| t.id == id).map(|t| host_of(&t.url)) else { return };
                if host.is_empty() || host == INTERNAL_HOST {
                    return; // Veyra's own pages keep the Veyra logo (shell.rs)
                }
                if let Some(pair) = self.ui.gfx.decode_icon(&png) {
                    let _ = std::fs::write(self.favicon_path(&host), &png);
                    self.icons.insert(host, pair);
                    self.ui.icons_changed();
                    self.redraw();
                }
            }
            Event::Audio(id, playing) => {
                if let Some(t) = self.tab_mut(id) {
                    t.playing_audio = playing;
                }
                self.redraw();
            }
            Event::Shortcut(a) => self.run(a),
            Event::PageMenu(id, args, deferral) => self.open_page_menu(id, args, deferral),
            Event::SettingsPage(id, json) => self.settings_message(id, &json),
            Event::NewWindow(url, args, deferral) => {
                // Middle click or Ctrl+click on a link: open it without leaving this tab.
                let background = BACKGROUND_CLICK
                    .with(|c| c.take())
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
                let id = self.add_tab(url, String::new(), false);
                if let Some(t) = self.tab_mut(id) {
                    t.pending_popup = Some((args, deferral));
                }
                if background && let Some(opener) = self.active {
                    // After the previous background tab from this one, if any.
                    if let Some((o, last)) = self.background_run
                        && o == opener
                        && let (Some(from), Some(after)) = (self.index(id), self.index(last))
                    {
                        let tab = self.tabs.remove(from);
                        let at = if after > from { after } else { after + 1 };
                        self.tabs.insert(at, tab);
                    }
                    self.background_run = Some((opener, id));
                    self.create_view(id);
                    self.redraw();
                } else {
                    self.activate(id);
                }
            }
            Event::Crashed(id) => {
                eprintln!("tab {id}: renderer gone, reloading");
                self.unload(id);
                if self.active == Some(id) {
                    self.create_view(id);
                }
            }
        }
    }

    /// Runs a shortcut action. Returns false for actions not handled yet,
    /// which are then left to WebView2's own handling.
    pub fn run(&mut self, action: Action) {
        use Action::*;
        let active = self.active_index();
        let len = self.tabs.len();
        let webview = active.and_then(|i| self.tabs[i].view.as_ref()).map(|v| v.webview.clone());
        let go = |app: &mut Self, i: usize| {
            if let Some(t) = app.tabs.get(i) {
                let id = t.id;
                app.activate(id);
            }
        };
        match action {
            NewTab if self.settings.new_tab_page => {
                BAR_PENDING.with(|p| p.set(false));
                TYPED_AHEAD.with(|t| t.borrow_mut().clear());
                self.open(NEWTAB_URL.into());
            }
            NewTab => self.open_bar(true),
            FocusAddress | CommandBar => self.open_bar(false),
            Bookmark => self.toggle_bookmark(),
            Settings => self.open_settings(),
            ToggleSidebar => self.toggle_sidebar(),
            CloseTab => {
                if let Some(i) = active {
                    let id = self.tabs[i].id;
                    self.close(id);
                }
            }
            ReopenClosedTab => {
                if let Some(t) = self.closed.pop() {
                    let id = self.add_tab(t.url, t.title, t.pinned);
                    self.activate(id);
                }
            }
            NextTab if len > 0 => go(self, active.map_or(0, |i| (i + 1) % len)),
            PrevTab if len > 0 => go(self, active.map_or(0, |i| (i + len - 1) % len)),
            Tab1 | Tab2 | Tab3 | Tab4 | Tab5 | Tab6 | Tab7 | Tab8 => {
                let n = [Tab1, Tab2, Tab3, Tab4, Tab5, Tab6, Tab7, Tab8].iter().position(|a| *a == action);
                go(self, n.unwrap_or(0));
            }
            LastTab if len > 0 => go(self, len - 1),
            Reload => {
                if let Some(wv) = webview {
                    unsafe {
                        let _ = wv.Reload();
                    }
                }
            }
            HardReload => {
                if let Some(wv) = webview {
                    let _ = unsafe {
                        wv.CallDevToolsProtocolMethod(w!("Page.reload"), w!(r#"{"ignoreCache":true}"#), None)
                    };
                }
            }
            Back => {
                if let Some(wv) = webview {
                    unsafe {
                        let _ = wv.GoBack();
                    }
                }
            }
            Forward => {
                if let Some(wv) = webview {
                    unsafe {
                        let _ = wv.GoForward();
                    }
                }
            }
            CopyUrl => {
                if let Some(i) = active {
                    copy_to_clipboard(self.hwnd, &self.tabs[i].url);
                }
            }
            PinTab => {
                if let Some(i) = active {
                    self.tabs[i].pinned = !self.tabs[i].pinned;
                    self.session_dirty = true;
                    self.redraw();
                }
            }
            DevTools => self.toggle_devtools(),
            // Need the UI (next phase): address bar, command bar, sidebar,
            // shortcut list. Find/zoom are left to WebView2 (see is_host_handled).
            _ => {}
        }
    }

    pub fn toggle_bookmark(&mut self) {
        if let Some(i) = self.active_index() {
            let (url, title) = (self.tabs[i].url.clone(), self.tabs[i].title.clone());
            self.store.borrow_mut().toggle_bookmark(&url, &title);
            self.redraw();
        }
    }

    pub(crate) fn navigate_active(&mut self, url: String) {
        match self.active_index() {
            Some(i) => {
                self.tabs[i].url = url.clone();
                match &self.tabs[i].view {
                    Some(v) => unsafe {
                        let _ = v.webview.Navigate(&HSTRING::from(url));
                    },
                    None => {
                        let id = self.tabs[i].id;
                        if self.tabs[i].state != TabState::Loading {
                            self.create_view(id);
                        }
                    }
                }
            }
            None => self.open(url),
        }
        self.focus_page();
    }

    // ---------- settings ----------

    /// Switches to the settings tab, opening it if needed.
    pub fn open_settings(&mut self) {
        match self.tabs.iter().find(|t| t.url.starts_with(SETTINGS_URL)).map(|t| t.id) {
            Some(id) => self.activate(id),
            None => self.open(SETTINGS_URL.into()),
        }
    }


    fn settings_message(&mut self, id: TabId, json: &str) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(json) else { return };
        let mut restart = false;
        match msg.get("type").and_then(|t| t.as_str()) {
            Some("get") => {}
            Some("set") => {
                let Some(new) = msg.get("settings").and_then(|s| serde_json::from_value::<Settings>(s.clone()).ok())
                else {
                    return;
                };
                restart = self.apply_settings(new);
            }
            Some("reset") => restart = self.apply_settings(Settings::default()),
            // Settings > About: look for an update / install it (update.rs).
            Some("upd_check") => {
                crate::update::check(self.hwnd);
                return;
            }
            Some("upd_install") => {
                crate::update::update(self.hwnd);
                return;
            }
            // New tab background: the Windows "Open" dialog (newtab.rs).
            Some("bg_pick") => {
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), crate::newtab::WM_BG_PICK, WPARAM(0), LPARAM(0));
                }
                return;
            }
            // New tab page search box.
            Some("ext_list" | "ext_enable" | "ext_remove" | "ext_open" | "ext_store" | "ext_folder") => return self.extensions_message(id, &msg),
            Some("dl_list" | "dl_open" | "dl_show" | "dl_folder" | "dl_clear") => return self.downloads_message(id, &msg),
            Some("go") => {
                if let Some(q) = msg.get("q").and_then(|q| q.as_str()) {
                    let url = self.settings.resolve_input(q);
                    self.activate(id);
                    self.navigate_active(url);
                    self.focus_page();
                }
                return;
            }
            _ => return,
        }
        let set = msg.get("type").and_then(|t| t.as_str()) == Some("set");
        let reply = self.settings_reply(if set { "saved" } else { "settings" }, restart);
        if let Some(v) = self.tabs.iter().find(|t| t.id == id).and_then(|t| t.view.as_ref()) {
            unsafe {
                let _ = v.webview.PostWebMessageAsJson(&HSTRING::from(reply));
            }
        }
        // The other open Veyra pages (settings, new tabs) follow the change.
        if set {
            self.notify_internal_pages(Some(id));
        }
    }

    fn settings_reply(&self, kind: &str, restart: bool) -> String {
        serde_json::json!({
            "type": kind,
            "settings": self.settings,
            "dark": self.ui.theme.dark,
            "restart": restart,
            "version": env!("CARGO_PKG_VERSION"),
            "lang": crate::i18n::code(),
            "update": crate::update::status_json(),
        })
        .to_string()
    }

    /// Sends the current settings to Veyra's open pages (except `skip`).
    pub(crate) fn notify_internal_pages(&self, skip: Option<TabId>) {
        let reply = HSTRING::from(self.settings_reply("settings", self.restart_pending));
        for t in self.tabs.iter().filter(|t| Some(t.id) != skip && t.url.starts_with(&format!("https://{INTERNAL_HOST}/"))) {
            if let Some(v) = &t.view {
                unsafe {
                    let _ = v.webview.PostWebMessageAsJson(&reply);
                }
            }
        }
    }

    /// Applies new settings live and saves them. Returns true when some
    /// change needs a restart.
    pub(crate) fn apply_settings(&mut self, mut new: Settings) -> bool {
        new.sanitize();
        new.save(&self.data_dir);
        // The other windows follow (and their open Veyra pages).
        for w in crate::other_windows(self.hwnd) {
            let copy = new.clone();
            crate::with_window(w, |a| {
                a.use_settings(copy);
                a.notify_internal_pages(None);
                a.redraw();
            });
        }
        self.use_settings(new)
    }

    /// Takes settings already saved (by this window or another one).
    fn use_settings(&mut self, new: Settings) -> bool {
        let restart = self.settings.restart_differs(&new) || self.restart_pending;
        self.restart_pending = restart;
        *self.keymap.borrow_mut() = Keymap::new(&new.shortcuts);
        self.settings = new;
        for t in &self.tabs {
            if let Some(v) = &t.view
                && let Ok(s) = unsafe { v.webview.Settings() }
            {
                apply_privacy(&v.webview, &s, &self.settings);
            }
        }
        crate::i18n::set(&self.settings.language);
        self.ui.width = self.settings.sidebar_width;
        self.ui.show_nav = self.settings.show_nav_buttons;
        self.ui.set_mode(&self.settings.sidebar);
        if self.ui.window_buttons == self.settings.title_bar {
            self.ui.window_buttons = !self.settings.title_bar;
            crate::set_frame(self.hwnd, self.settings.title_bar);
        }
        self.apply_theme();
        self.layout();
        restart
    }

    /// Theme and backdrop from settings + Windows (dark mode, transparency).
    pub fn apply_theme(&mut self) {
        let dark = self.settings.dark(crate::system_dark_mode());
        let glass = self.settings.glass && crate::system_transparency() && !self.comp.software;
        self.ui.theme = crate::ui::Theme { dark, glass };
        crate::apply_backdrop(self.hwnd, self.ui.theme);
    }

    // ---------- groups ----------

    /// Keeps each group's tabs next to each other (after the first one).
    fn regroup(&mut self) {
        let mut out: Vec<Tab> = Vec::with_capacity(self.tabs.len());
        for tab in std::mem::take(&mut self.tabs) {
            match tab.group {
                Some(g) if !tab.pinned => match out.iter().rposition(|t| t.group == Some(g)) {
                    Some(last) => out.insert(last + 1, tab),
                    None => out.push(tab),
                },
                _ => out.push(tab),
            }
        }
        self.tabs = out;
        let used: Vec<u32> = self.tabs.iter().filter_map(|t| t.group).collect();
        self.groups.retain(|g| used.contains(&g.id));
        self.session_dirty = true;
    }

    pub fn new_group(&mut self, tab: TabId) -> u32 {
        let id = self.groups.iter().map(|g| g.id).max().unwrap_or(0) + 1;
        let name = format!("{} {}", crate::i18n::t("Gruppo", "Group"), id);
        self.groups.push(Group { id, name, collapsed: false });
        self.set_group(tab, Some(id));
        id
    }

    pub fn set_group(&mut self, tab: TabId, group: Option<u32>) {
        if let Some(t) = self.tab_mut(tab) {
            t.group = group;
            t.pinned = false;
        }
        self.regroup();
    }

    pub fn toggle_group(&mut self, g: u32) {
        if let Some(gr) = self.groups.iter_mut().find(|x| x.id == g) {
            gr.collapsed = !gr.collapsed;
            self.session_dirty = true;
        }
    }

    pub fn rename_group(&mut self, g: u32, name: &str) {
        let name = name.trim();
        if let Some(gr) = self.groups.iter_mut().find(|x| x.id == g)
            && !name.is_empty()
        {
            gr.name = name.chars().take(40).collect();
            self.session_dirty = true;
        }
    }

    /// "Separa": tabs stay, the group goes.
    pub fn dissolve_group(&mut self, g: u32) {
        for t in self.tabs.iter_mut().filter(|t| t.group == Some(g)) {
            t.group = None;
        }
        self.regroup();
    }

    pub fn close_group(&mut self, g: u32) {
        let ids: Vec<TabId> = self.tabs.iter().filter(|t| t.group == Some(g)).map(|t| t.id).collect();
        for id in ids {
            self.close(id);
        }
        self.regroup();
    }

    // ---------- session ----------

    fn session_path(&self) -> PathBuf {
        self.data_dir.join("session.json")
    }

    pub fn restore_session(&mut self) -> bool {
        if !self.settings.restore_session {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(self.session_path()) else { return false };
        let Ok(session) = serde_json::from_str::<Session>(&text) else { return false };
        self.groups = session.groups;
        let mut ids = Vec::new();
        for t in session.tabs {
            if t.pinned && self.pins_loaded {
                continue; // an old session: they are in pins.json now
            }
            let id = self.add_tab(t.url, t.title, t.pinned);
            if let Some(tab) = self.tab_mut(id) {
                tab.group = t.group;
            }
            ids.push(id);
        }
        // Drop groups that no tab uses any more.
        let used: Vec<u32> = self.tabs.iter().filter_map(|t| t.group).collect();
        self.groups.retain(|g| used.contains(&g.id));
        let pin = session.active_pin.and_then(|p| self.tabs.iter().find(|t| t.pinned && t.pin == p)).map(|t| t.id);
        if let Some(id) = pin.or_else(|| ids.get(session.active).or(ids.first()).copied()) {
            self.activate(id); // only this one loads; others stay unloaded
            return true;
        }
        false
    }

    /// Every window saves its tabs (the last one closed is what opens next
    /// time); the pinned ones go to `pins.json`.
    pub fn save_session(&mut self) {
        let normal: Vec<&Tab> = self
            .tabs
            .iter()
            .filter(|t| !t.pinned && Some(t.id) != self.incoming && !t.url.is_empty() && t.url != "about:blank")
            .collect();
        let active_pin = self.active.and_then(|a| self.tabs.iter().find(|t| t.id == a && t.pinned)).map(|t| t.pin);
        let session = Session {
            active: self.active.and_then(|a| normal.iter().position(|t| t.id == a)).unwrap_or(0),
            tabs: normal
                .iter()
                .map(|t| SavedTab { url: t.url.clone(), title: t.title.clone(), pinned: false, group: t.group })
                .collect(),
            active_pin,
            groups: self.groups.clone(),
        };
        self.save_pin_details();
        let path = self.session_path();
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_string(&session).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        self.session_dirty = false;
    }

    pub fn shutdown(&mut self) {
        self.end_incoming();
        self.hand_over_closed();
        crate::placement::save(self.hwnd, &self.data_dir, self.fullscreen);
        self.save_session();
        self.store.borrow_mut().save();
        for t in &mut self.tabs {
            if let Some(v) = t.view.take() {
                unsafe {
                    let _ = v.controller.Close();
                }
            }
        }
    }
}

/// Actions the host implements itself (others fall through to WebView2).
pub(crate) fn is_host_handled(a: Action) -> bool {
    !matches!(
        a,
        Action::Find
            | Action::ZoomIn
            | Action::ZoomOut
            | Action::ZoomReset
            | Action::ShowShortcuts
    )
}

pub(crate) fn views<'a>(
    tabs: &'a [Tab],
    icons: &'a HashMap<String, (IWICBitmap, IWICBitmap)>,
    active: Option<TabId>,
) -> Vec<TabView<'a>> {
    tabs.iter()
        .map(|t| TabView {
            id: t.id,
            title: &t.title,
            url: &t.url,
            pinned: t.pinned,
            active: Some(t.id) == active,
            unloaded: t.view.is_none(),
            audio: t.playing_audio,
            group: t.group,
            icon: icons.get(&host_of(&t.url)),
        })
        .collect()
}

pub fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split(['/', '?', '#']).next())
        .map(|h| h.rsplit('@').next().unwrap_or(h).split(':').next().unwrap_or("").to_ascii_lowercase())
        .unwrap_or_default()
}

/// Per-WebView privacy settings (tracking prevention is profile-wide;
/// setting it again per tab is harmless and picks up changes).
fn apply_privacy(webview: &ICoreWebView2, settings: &ICoreWebView2Settings, prefs: &Settings) {
    let level = match prefs.tracking_prevention.as_str() {
        "strict" => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT,
        "basic" => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BASIC,
        "off" => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_NONE,
        _ => COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_BALANCED,
    };
    unsafe {
        if let Ok(profile) = webview.cast::<ICoreWebView2_13>().and_then(|w| w.Profile())
            && let Ok(p3) = profile.cast::<ICoreWebView2Profile3>()
        {
            let _ = p3.SetPreferredTrackingPreventionLevel(level);
        }
        if let Ok(s4) = settings.cast::<ICoreWebView2Settings4>() {
            let _ = s4.SetIsPasswordAutosaveEnabled(prefs.save_passwords);
            let _ = s4.SetIsGeneralAutofillEnabled(prefs.autofill);
        }
        // No Microsoft SmartScreen ("not commonly downloaded", "report as
        // safe"...): risky sites are caught by the block lists instead.
        if let Ok(s8) = settings.cast::<ICoreWebView2Settings8>() {
            let _ = s8.SetIsReputationCheckingRequired(false);
        }
    }
}

/// Low memory target for hidden tabs lets Chromium drop caches.
fn set_memory_level(webview: &ICoreWebView2, low: bool) {
    if let Ok(wv) = webview.cast::<ICoreWebView2_19>() {
        let level = if low {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
        } else {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
        };
        unsafe {
            let _ = wv.SetMemoryUsageTargetLevel(level);
        }
    }
}

fn wire_events(
    id: TabId,
    hwnd: Owner,
    controller: &ICoreWebView2Controller,
    webview: &ICoreWebView2,
    keymap: Rc<RefCell<Keymap>>,
) -> Result<()> {
    let mut token = 0;
    unsafe {
        webview.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |wv, _| {
                if let Some(wv) = wv {
                    let mut p = PWSTR::null();
                    wv.DocumentTitle(&mut p)?;
                    post(hwnd, Event::Title(id, CoTaskMemPWSTR::from(p).to_string()));
                }
                Ok(())
            })),
            &mut token,
        )?;
        if sys::perf_on() {
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                    post(hwnd, Event::NavStart(id));
                    Ok(())
                })),
                &mut token,
            )?;
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, _| {
                    post(hwnd, Event::NavDone(id));
                    Ok(())
                })),
                &mut token,
            )?;
        }
        webview.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |wv, _| {
                if let Some(wv) = wv {
                    let mut p = PWSTR::null();
                    wv.Source(&mut p)?;
                    post(hwnd, Event::Url(id, CoTaskMemPWSTR::from(p).to_string()));
                }
                Ok(())
            })),
            &mut token,
        )?;
        crate::fullscreen::watch(webview, id, hwnd)?;
        crate::downloads::watch(webview, id, hwnd)?;
        // The address being loaded: when loading fails the page is still the
        // previous one (or about:blank), so it is remembered here.
        let loading = Rc::new(RefCell::new(String::new()));
        let l2 = loading.clone();
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut p = PWSTR::null();
                    args.Uri(&mut p)?;
                    *l2.borrow_mut() = CoTaskMemPWSTR::from(p).to_string();
                }
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut ok = BOOL(0);
                args.IsSuccess(&mut ok)?;
                if ok.as_bool() {
                    return Ok(());
                }
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
                args.WebErrorStatus(&mut status)?;
                let kind = match status {
                    // Cancelled by us (ad blocker) or turned into a download.
                    COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED => return Ok(()),
                    COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED => "offline",
                    COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => "dns",
                    COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => "timeout",
                    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET | COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE | COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT => "refused",
                    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
                    | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID => "cert",
                    _ => "generic",
                };
                let url = loading.borrow().clone();
                if url.starts_with("http") {
                    post(hwnd, Event::LoadFailed(id, url, kind));
                }
                Ok(())
            })),
            &mut token,
        )?;
        // Permission requests get our own card instead of WebView2's bubble.
        webview.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let deferral = args.GetDeferral()?;
                    post(hwnd, Event::Permission(id, args, deferral));
                }
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    // Already refused by the ad blocker.
                    let mut handled = BOOL(0);
                    args.Handled(&mut handled)?;
                    if handled.as_bool() {
                        return Ok(());
                    }
                    // Popups a page opens on its own (no click or key from
                    // the user) are blocked, as in every browser.
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    let mut p = PWSTR::null();
                    args.Uri(&mut p)?;
                    let url = CoTaskMemPWSTR::from(p).to_string();
                    if !user.as_bool() {
                        args.SetHandled(true)?;
                        eprintln!("popup without user action blocked: {url}");
                        return Ok(());
                    }
                    let deferral = args.GetDeferral()?;
                    post(hwnd, Event::NewWindow(url, args, deferral));
                }
                Ok(())
            })),
            &mut token,
        )?;
        if let Ok(wv11) = webview.cast::<ICoreWebView2_11>() {
            wv11.add_ContextMenuRequested(
                &ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let deferral = args.GetDeferral()?;
                    args.SetHandled(true)?;
                    post(hwnd, Event::PageMenu(id, args, deferral));
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // Messages from the internal settings page (only that origin is trusted).
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Source(&mut p)?;
                let source = CoTaskMemPWSTR::from(p).to_string();
                if !source.starts_with(&format!("https://{INTERNAL_HOST}/")) {
                    return Ok(());
                }
                let mut p = PWSTR::null();
                args.WebMessageAsJson(&mut p)?;
                post(hwnd, Event::SettingsPage(id, CoTaskMemPWSTR::from(p).to_string()));
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
                    args.ProcessFailedKind(&mut kind)?;
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
                    {
                        post(hwnd, Event::Crashed(id));
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        if let Ok(wv15) = webview.cast::<ICoreWebView2_15>() {
            wv15.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |wv, _| {
                    let Some(wv15) = wv.and_then(|w| w.cast::<ICoreWebView2_15>().ok()) else { return Ok(()) };
                    let handler = GetFaviconCompletedHandler::create(Box::new(move |err, stream| {
                        if let (Ok(()), Some(stream)) = (err, stream) {
                            let png = read_stream(&stream);
                            if !png.is_empty() {
                                post(hwnd, Event::Favicon(id, png));
                            }
                        }
                        Ok(())
                    }));
                    wv15.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &handler)?;
                    Ok(())
                })),
                &mut token,
            )?;
        }
        if let Ok(wv8) = webview.cast::<ICoreWebView2_8>() {
            wv8.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |wv, _| {
                    if let Some(wv8) = wv.and_then(|w| w.cast::<ICoreWebView2_8>().ok()) {
                        let mut playing = false.into();
                        wv8.IsDocumentPlayingAudio(&mut playing)?;
                        post(hwnd, Event::Audio(id, playing.as_bool()));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        // Shortcuts work even while the page has keyboard focus.
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_UP;
                args.KeyEventKind(&mut kind)?;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    return Ok(());
                }
                let mut vk = 0u32;
                args.VirtualKey(&mut vk)?;
                if let Some(action) = keymap.borrow().lookup(vk as u16)
                    && is_host_handled(action)
                {
                    args.SetHandled(true)?;
                    if matches!(action, Action::NewTab | Action::FocusAddress | Action::CommandBar) {
                        // Take the keyboard now so the next keys reach the bar.
                        BAR_PENDING.with(|p| p.set(true));
                        if let Some(w) = tab_owner(hwnd.0) {
                            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(w));
                        }
                    }
                    post(hwnd, Event::Shortcut(action));
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

fn read_stream(stream: &windows::Win32::System::Com::IStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let mut read = 0u32;
        let hr = unsafe { stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut read)) };
        if hr.is_err() || read == 0 || out.len() > 1 << 20 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
    }
    out
}

pub(crate) fn paste_from_clipboard(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::DataExchange::GetClipboardData;
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let mem = windows::Win32::Foundation::HGLOBAL(h.0);
            let ptr = GlobalLock(mem) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 && len < 1 << 20 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

pub(crate) fn copy_to_clipboard(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let ptr = GlobalLock(mem) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(mem);
                const CF_UNICODETEXT: u32 = 13;
                let _ = SetClipboardData(CF_UNICODETEXT, Some(windows::Win32::Foundation::HANDLE(mem.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// Keymap lookup for keys pressed while the main window itself has focus.
pub fn window_shortcut(app: &App, vk: u16) -> Option<Action> {
    app.keymap.borrow().lookup(vk).filter(|a| is_host_handled(*a))
}

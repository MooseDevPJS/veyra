//! Everything drawn by the browser itself: the sidebar ("chrome") and the
//! overlay (command bar, shortcut tooltips).
//!
//! Layout is in DIPs. One function lays out the sidebar and is used both to
//! paint and to hit-test, so the two can never disagree.

use std::collections::HashMap;

use windows::{
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Imaging::IWICBitmap,
    },
    core::*,
};

use crate::{
    anim::Tween,
    i18n::{t, t as tr},
    app::TabId,
    gfx::{Gfx, Icon, contains, draw_icon, rect, rgba},
    store::Suggestion,
    textfield::TextField,
};

// ---------- theme ----------

#[derive(Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// Acrylic backdrop behind the sidebar (false: solid frame).
    pub glass: bool,
}

impl Theme {
    /// Main text.
    pub fn fg(&self) -> u32 {
        if self.dark { 0xF2F2F4 } else { 0x1C1C1E }
    }
    /// Tint base for hover/selection: white on dark, black on light.
    pub fn ink(&self) -> u32 {
        if self.dark { 0xFFFFFF } else { 0x000000 }
    }
    /// Opaque frame color (solid mode, and the floating sidebar over the page).
    pub fn frame(&self) -> u32 {
        if self.dark { 0x111113 } else { 0xE9E9EC }
    }
    /// Floating panels: command bar, tooltips.
    pub fn panel(&self) -> u32 {
        if self.dark { 0x1F1F22 } else { 0xFBFBFC }
    }
}

// ---------- sidebar ----------

pub const PAD: f32 = 10.0;
const BTN: f32 = 30.0;
const URL_H: f32 = 34.0;
const TILE_H: f32 = 42.0;
const TAB_H: f32 = 34.0;
const ROW_GAP: f32 = 2.0;
pub const PAGE_INSET: f32 = 8.0;
pub const PAGE_RADIUS: f32 = 12.0;
/// Compact sidebar strip: site icons only.
pub const COMPACT_W: f32 = 54.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Hit {
    Back,
    Forward,
    Reload,
    Minimize,
    Maximize,
    Close,
    Url,
    Tile(TabId),
    Tab(TabId),
    CloseTab(TabId),
    NewTab,
    Settings,
    /// Puzzle in the address pill: the extensions menu.
    Extensions,
    /// Star in the address pill: bookmark the current page.
    Star,
    /// Bookmark icon (index into `Ui::bookmarks`).
    Bookmark(usize),
    /// Last bookmark slot when they do not fit in two rows: opens a list.
    MoreBookmarks,
    /// Next to the settings gear, for a while after a download.
    Downloads,
    /// Group header row in the tab list.
    Group(u32),
    /// Empty sidebar area: drags the window.
    Caption,
}

const GROUP_H: f32 = 30.0;
const GROUP_INDENT: f32 = 12.0;
const BM: f32 = 30.0;
const BM_GAP: f32 = 6.0;

pub struct TabView<'a> {
    pub id: TabId,
    pub title: &'a str,
    pub url: &'a str,
    pub pinned: bool,
    pub active: bool,
    pub unloaded: bool,
    pub audio: bool,
    pub group: Option<u32>,
    pub icon: Option<&'a (IWICBitmap, IWICBitmap)>,
}

/// A tab being dragged in the list.
pub struct Drag {
    pub id: TabId,
    pub start_y: f32,
    pub y: f32,
    pub x: f32,
    /// Where it was grabbed (DIPs): a torn-off window keeps the tab there
    /// under the pointer.
    pub start_x: f32,
    /// Moved far enough to count as a drag (not a click).
    pub moving: bool,
}

pub struct CommandBar {
    pub open: bool,
    pub t: Tween,
    pub field: TextField,
    pub items: Vec<Suggestion>,
    pub sel: usize,
    pub hover: Option<usize>,
    /// Open the result in a new tab (Ctrl+T) instead of the current one (Ctrl+L).
    pub new_tab: bool,
    /// Renaming this group instead of navigating.
    pub rename: Option<u32>,
    pub caret_blink: std::time::Instant,
}

#[derive(Clone, PartialEq, Debug)]
pub enum MenuCmd {
    // Tab menu.
    Pin,
    Duplicate,
    Reload,
    CopyUrl,
    NewWindow,
    Close,
    CloseOthers,
    CloseBelow,
    // Groups.
    NewGroup,
    MoveToGroup(u32),
    Ungroup,
    RenameGroup(u32),
    DissolveGroup(u32),
    CloseGroup(u32),
    // Page menu: a WebView2 built-in command (copy, paste, save image...).
    Builtin(i32),
    /// Built-in "Inspect": also docks the DevTools that opens.
    Inspect(i32),
    OpenTab(String),
    OpenWindow(String),
    CopyText(String),
    SearchText(String),
    Back,
    Forward,
    PageReload,
    // Bookmark menu.
    OpenHere(String),
    RemoveBookmark(String),
    // Command bar text field.
    Bar(BarEdit),
    // Image under the pointer (address from App::context_image).
    OpenImage,
    DownloadImage,
    // Downloads quick menu.
    OpenDownload(usize),
    DownloadMenu(usize),
    DownloadOpenWhenDone(usize),
    DownloadPause(usize),
    DownloadCancel(usize),
    DownloadsFolder,
    AllDownloads,
    // Extensions button.
    ExtPick(usize),
    ExtSite(usize, Option<bool>),
    ExtOpen(usize),
    ExtStore,
    ManageExtensions,
}

/// Editing commands of the command bar's field (keys and right-click menu).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BarEdit {
    Cut,
    Copy,
    Paste,
    PasteGo,
    SelectAll,
    Undo,
}

/// The page's pending context menu request, completed when the menu closes.
pub struct PageMenu {
    pub args: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2ContextMenuRequestedEventArgs,
    pub deferral: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Deferral,
}

pub struct MenuItem {
    pub label: String,
    pub cmd: MenuCmd,
    pub keys: String,
    /// Draw a separator line above this item.
    pub sep: bool,
}

/// Right-click menu of a tab.
pub struct Menu {
    pub tab: TabId,
    pub x: f32,
    pub y: f32,
    pub items: Vec<MenuItem>,
    pub hover: Option<usize>,
    pub t: Tween,
    /// Set for a page (right-click inside the web page) menu.
    pub page: Option<PageMenu>,
}

const MENU_ROW: f32 = 32.0;
const MENU_W: f32 = 256.0;

pub struct Tip {
    pub label: String,
    pub keys: Vec<String>,
    /// Anchor in DIPs (tooltip sits to the right of / below it).
    pub x: f32,
    pub y: f32,
    pub t: Tween,
}

pub struct Ui {
    pub gfx: Gfx,
    pub theme: Theme,
    pub width: f32,
    pub visible: bool,
    /// Compact sidebar: a strip of site icons that expands over the page
    /// on hover (`peek` animates the expansion).
    pub compact: bool,
    /// Hidden sidebar revealed by hovering the left edge (floats over the page).
    pub peek: Tween,
    /// History panel coming in (1) or going out (0): see history.rs.
    pub history_t: Tween,
    pub show_nav: bool,
    /// Minimize/maximize/close in the sidebar (off with the classic title bar).
    pub window_buttons: bool,
    pub maximized: bool,
    /// Pointer on the resizable right edge: a grip line shows there.
    pub edge_hover: bool,
    /// Tab groups: (id, name, collapsed).
    pub groups: Vec<(u32, String, bool)>,
    /// Bookmarks shown above the pinned tiles: (title, url).
    pub bookmarks: Vec<(String, String)>,
    /// The current page is bookmarked (filled star).
    pub bookmarked: bool,
    pub has_page: bool,
    /// The downloads quick button is shown (recent or running download).
    pub downloads_button: bool,
    /// Download card flying into that button (0 = resting, 1 = gone).
    pub card_out: Tween,
    pub hover: Option<Hit>,
    hover_t: HashMap<Hit, Tween>,
    /// Y of the selection highlight in the tab list (slides between rows).
    sel_y: Tween,
    sel_id: Option<TabId>,
    pub scroll: f32,
    /// Tab to scroll into view at the next render (after activation).
    pub reveal: Option<TabId>,
    pub drag: Option<Drag>,
    pub bar: CommandBar,
    pub tip: Option<Tip>,
    pub menu: Option<Menu>,
    /// Where the last plain menu opened (a sub-menu opens in the same place).
    pub menu_anchor: (f32, f32),
    fonts: Fonts,
    bitmaps: HashMap<(usize, bool), ID2D1Bitmap>,
    pub anims: bool,
}

struct Fonts {
    title: IDWriteTextFormat,
    title_bold: IDWriteTextFormat,
    small: IDWriteTextFormat,
    url: IDWriteTextFormat,
    field: IDWriteTextFormat,
    chip: IDWriteTextFormat,
}

impl Ui {
    pub fn new(gfx: Gfx, theme: Theme) -> Result<Self> {
        let fonts = Fonts {
            title: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            title_bold: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            small: gfx.text_format(12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            url: gfx.text_format(12.5, DWRITE_FONT_WEIGHT_NORMAL)?,
            field: gfx.text_format(16.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            chip: gfx.text_format(11.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        };
        unsafe {
            let _ = fonts.chip.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = fonts.url.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        }
        Ok(Ui {
            gfx,
            theme,
            width: 260.0,
            visible: true,
            compact: false,
            peek: Tween::new(0.0),
            history_t: Tween::new(0.0),
            show_nav: true,
            window_buttons: true,
            maximized: false,
            edge_hover: false,
            groups: Vec::new(),
            bookmarks: Vec::new(),
            bookmarked: false,
            has_page: false,
            downloads_button: false,
            card_out: Tween::new(0.0),
            hover: None,
            hover_t: HashMap::new(),
            sel_y: Tween::new(0.0),
            sel_id: None,
            scroll: 0.0,
            reveal: None,
            drag: None,
            bar: CommandBar {
                open: false,
                t: Tween::new(0.0),
                field: TextField::default(),
                items: Vec::new(),
                sel: 0,
                hover: None,
                new_tab: false,
                rename: None,
                caret_blink: std::time::Instant::now(),
            },
            tip: None,
            menu: None,
            menu_anchor: (0.0, 0.0),
            fonts,
            bitmaps: HashMap::new(),
            anims: true,
        })
    }

    fn ms(&self, ms: u64) -> u64 {
        if self.anims { ms } else { ms.min(90) }
    }

    pub fn icons_changed(&mut self) {
        self.bitmaps.clear();
    }

    /// True while something is moving (the window then redraws each frame).
    pub fn animating(&self) -> bool {
        self.card_out.running()
            || self.sel_y.running()
            || self.peek.running()
            || self.history_t.running()
            || self.bar.t.running()
            || self.tip.as_ref().is_some_and(|t| t.t.running())
            || self.menu.as_ref().is_some_and(|m| m.t.running())
            || self.hover_t.values().any(|t| t.running())
    }

    pub fn set_hover(&mut self, hit: Option<Hit>) {
        if hit == self.hover {
            return;
        }
        let (fade_in, fade_out) = (self.ms(90), self.ms(160));
        if let Some(old) = self.hover {
            self.hover_t.entry(old).or_insert_with(|| Tween::new(1.0)).go(0.0, fade_out);
        }
        if let Some(new) = hit {
            self.hover_t.entry(new).or_insert_with(|| Tween::new(0.0)).go(1.0, fade_in);
        }
        self.hover = hit;
        self.hover_t.retain(|h, t| Some(*h) == hit || t.target() > 0.0 || t.running());
    }

    fn hover_amount(&self, hit: Hit) -> f32 {
        self.hover_t.get(&hit).map_or(0.0, |t| t.value())
    }

    /// "docked", "compact" or "hidden" (see Settings::sidebar).
    pub fn set_mode(&mut self, mode: &str) {
        let (visible, compact) = (mode == "docked", mode == "compact");
        if (visible, compact) != (self.visible, self.compact) {
            self.visible = visible;
            self.compact = compact;
            self.peek.set(0.0);
        }
    }

    /// Sidebar x offset in DIPs: 0 when docked, compact or fully peeked,
    /// -width when hidden.
    pub fn offset(&self) -> f32 {
        if self.visible || self.compact { 0.0 } else { -self.width * (1.0 - self.peek.value()) }
    }

    /// Width the sidebar takes from the page (DIPs): 0 when hidden.
    pub fn docked_width(&self) -> f32 {
        if self.visible {
            self.width
        } else if self.compact {
            COMPACT_W
        } else {
            0.0
        }
    }

    /// Right edge (DIPs) of a sidebar floating over the page now, 0 if none.
    pub fn floating_right(&self) -> f32 {
        let p = self.peek.value();
        if self.visible || p <= 0.001 {
            0.0
        } else if self.compact {
            COMPACT_W + (self.width - COMPACT_W) * p
        } else {
            self.width + self.offset()
        }
    }

    /// The full sidebar floats over the page (hidden or compact mode, hovered).
    pub fn floating(&self) -> bool {
        !self.visible && self.peek.target() > 0.5
    }

    pub fn url_rect(&self) -> D2D_RECT_F {
        let top = PAD + BTN + 6.0;
        rect(PAD, top, self.width - PAD, top + URL_H)
    }

    /// Sidebar layout for `height` DIPs. Order = paint order.
    pub fn layout(&self, tabs: &[TabView], height: f32) -> Vec<(D2D_RECT_F, Hit)> {
        let w = self.width;
        let mut items = vec![(rect(0.0, 0.0, w, height), Hit::Caption)];
        if self.show_nav {
            for (i, hit) in [Hit::Back, Hit::Forward, Hit::Reload].into_iter().enumerate() {
                let x = PAD - 2.0 + i as f32 * BTN;
                items.push((rect(x, PAD, x + BTN, PAD + BTN), hit));
            }
        }
        if self.window_buttons {
            for (i, hit) in [Hit::Minimize, Hit::Maximize, Hit::Close].into_iter().enumerate() {
                let x = w - PAD + 2.0 - (3 - i) as f32 * BTN;
                items.push((rect(x, PAD, x + BTN, PAD + BTN), hit));
            }
        }
        items.push((self.url_rect(), Hit::Url));
        if self.has_page {
            let u = self.url_rect();
            items.push((rect(u.right - 30.0, u.top + 5.0, u.right - 6.0, u.bottom - 5.0), Hit::Star));
            items.push((rect(u.right - 56.0, u.top + 5.0, u.right - 32.0, u.bottom - 5.0), Hit::Extensions));
        }
        let (per_row, slots) = self.bookmark_grid();
        let more = self.bookmarks.len() > slots;
        for i in 0..slots {
            let (c, r) = ((i % per_row) as f32, (i / per_row) as f32);
            let x = PAD + c * (BM + BM_GAP);
            let by = self.url_rect().bottom + 10.0 + r * (BM + BM_GAP);
            let hit = if more && i == slots - 1 { Hit::MoreBookmarks } else { Hit::Bookmark(i) };
            items.push((rect(x, by, x + BM, by + BM), hit));
        }

        let mut y = self.grid_top();
        let mut pinned: Vec<_> = tabs.iter().filter(|t| t.pinned).collect();
        // A dragged tile shows in the slot it would drop into.
        if let Some(d) = self.drag.as_ref().filter(|d| d.moving)
            && let Some(from) = pinned.iter().position(|t| t.id == d.id)
        {
            let t = pinned.remove(from);
            let to = self.tile_slot(tabs, d.x, d.y).min(pinned.len());
            pinned.insert(to, t);
        }
        if !pinned.is_empty() {
            let cols = 3.0;
            let gap = 6.0;
            let tw = (w - 2.0 * PAD - (cols - 1.0) * gap) / cols;
            for (i, t) in pinned.iter().enumerate() {
                let (c, r) = ((i % 3) as f32, (i / 3) as f32);
                let x = PAD + c * (tw + gap);
                let ty = y + r * (TILE_H + gap);
                items.push((rect(x, ty, x + tw, ty + TILE_H), Hit::Tile(t.id)));
            }
            y += pinned.len().div_ceil(3) as f32 * (TILE_H + gap) + 8.0;
        }

        let bottom = height - PAD - BTN;
        items.push((rect(PAD - 2.0, bottom, PAD - 2.0 + BTN, bottom + BTN), Hit::Settings));
        if self.downloads_button {
            let x = PAD - 2.0 + BTN + 4.0;
            items.push((rect(x, bottom, x + BTN, bottom + BTN), Hit::Downloads));
        }
        let (_, tab_limit, sticky) = self.list_area(tabs, height);
        let mut ry = y - self.scroll;
        let mut current: Option<u32> = None;
        for t in self.list_order(tabs) {
            // A header before the first tab of each group.
            if t.group != current {
                current = t.group;
                if let Some(g) = t.group {
                    if ry + GROUP_H > y && ry < tab_limit {
                        items.push((rect(PAD, ry, w - PAD, ry + GROUP_H), Hit::Group(g)));
                    }
                    ry += GROUP_H + ROW_GAP;
                }
            }
            // Collapsed groups hide their tabs (except the active one).
            if t.group.is_some_and(|g| self.group_collapsed(g)) && !t.active {
                continue;
            }
            if ry + TAB_H > y && ry < tab_limit {
                let indent = if t.group.is_some() { GROUP_INDENT } else { 0.0 };
                let row = rect(PAD + indent, ry, w - PAD, ry + TAB_H);
                items.push((row, Hit::Tab(t.id)));
                items.push((rect(row.right - 28.0, ry + 4.0, row.right - 4.0, ry + TAB_H - 4.0), Hit::CloseTab(t.id)));
            }
            ry += TAB_H + ROW_GAP;
        }
        // "New tab" follows the last tab; when the list overflows it stays
        // pinned at the bottom and the tabs scroll above it.
        let ny = if sticky { tab_limit + ROW_GAP } else { ry };
        if ny + TAB_H > y {
            items.push((rect(PAD, ny, w - PAD, ny + TAB_H), Hit::NewTab));
        }
        items
    }

    /// Scrolls the tab list so tab `id` is fully visible.
    pub fn scroll_to(&mut self, tabs: &[TabView], height: f32, id: TabId) {
        let (top, bottom, _) = self.list_area(tabs, height);
        let mut y = top;
        let mut current: Option<u32> = None;
        let mut found = None;
        for t in self.list_order(tabs) {
            if t.group != current {
                current = t.group;
                if t.group.is_some() {
                    y += GROUP_H + ROW_GAP;
                }
            }
            if t.group.is_some_and(|g| self.group_collapsed(g)) && !t.active {
                continue;
            }
            if t.id == id {
                found = Some(y);
                break;
            }
            y += TAB_H + ROW_GAP;
        }
        let Some(row) = found else { return };
        let max = self.max_scroll(tabs, height);
        if row - self.scroll < top {
            self.scroll = (row - top).clamp(0.0, max);
        } else if row + TAB_H - self.scroll > bottom {
            self.scroll = (row + TAB_H - bottom).clamp(0.0, max);
        }
    }

    /// The scrolling tab list: (top, bottom) in DIPs, and whether it
    /// overflows (then "New tab" sits fixed below it).
    pub fn list_area(&self, tabs: &[TabView], height: f32) -> (f32, f32, bool) {
        let top = self.list_top(tabs);
        let list_bottom = height - PAD - BTN - 6.0;
        let rows = tabs.iter().filter(|t| !t.pinned && (t.active || !t.group.is_some_and(|g| self.group_collapsed(g)))).count();
        let content = rows as f32 * (TAB_H + ROW_GAP) + self.groups.len() as f32 * (GROUP_H + ROW_GAP);
        let sticky = top + content + TAB_H > list_bottom;
        let bottom = if sticky { list_bottom - TAB_H - ROW_GAP } else { list_bottom };
        (top, bottom, sticky)
    }

    /// Unpinned tabs in list order, with a dragged tab moved to where it
    /// would drop.
    fn list_order<'a>(&self, tabs: &'a [TabView<'a>]) -> Vec<&'a TabView<'a>> {
        let mut list: Vec<_> = tabs.iter().filter(|t| !t.pinned).collect();
        if let Some(d) = self.drag.as_ref().filter(|d| d.moving)
            && let Some(from) = list.iter().position(|t| t.id == d.id)
        {
            let t = list.remove(from);
            let to = self.drop_index(tabs, d.y).min(list.len());
            list.insert(to, t);
        }
        list
    }

    /// Index in the unpinned list where a tab dragged to `y` lands.
    pub fn drop_index(&self, tabs: &[TabView], y: f32) -> usize {
        let top = self.list_top(tabs);
        let n = tabs.iter().filter(|t| !t.pinned).count();
        (((y - top + self.scroll) / (TAB_H + ROW_GAP)).floor().max(0.0) as usize).min(n.saturating_sub(1))
    }

    /// Slot (index among the pinned tabs) of the tile grid under (x, y).
    pub fn tile_slot(&self, tabs: &[TabView], x: f32, y: f32) -> usize {
        let n = tabs.iter().filter(|t| t.pinned).count();
        let (gap, cols) = (6.0, 3usize);
        let tw = (self.width - 2.0 * PAD - (cols as f32 - 1.0) * gap) / cols as f32;
        let col = (((x - PAD) / (tw + gap)).floor().max(0.0) as usize).min(cols - 1);
        let row = ((y - self.grid_top()) / (TILE_H + gap)).floor().max(0.0) as usize;
        (row * cols + col).min(n.saturating_sub(1))
    }

    /// Bookmark icons per row and how many slots are used (max two rows;
    /// when they do not all fit, the last slot is the "more" button).
    fn bookmark_grid(&self) -> (usize, usize) {
        let per_row = (((self.width - 2.0 * PAD + BM_GAP) / (BM + BM_GAP)).floor() as usize).max(1);
        (per_row, self.bookmarks.len().min(per_row * 2))
    }

    /// Top of the pinned tiles (below the address pill and bookmarks).
    fn grid_top(&self) -> f32 {
        let (per_row, shown) = self.bookmark_grid();
        let rows = shown.div_ceil(per_row) as f32;
        self.url_rect().bottom + 12.0 + if shown > 0 { rows * (BM + BM_GAP) + 2.0 } else { 0.0 }
    }

    fn list_top(&self, tabs: &[TabView]) -> f32 {
        let pinned = tabs.iter().filter(|t| t.pinned).count();
        let mut y = self.grid_top();
        if pinned > 0 {
            y += pinned.div_ceil(3) as f32 * (TILE_H + 6.0) + 8.0;
        }
        y
    }

    pub fn max_scroll(&self, tabs: &[TabView], height: f32) -> f32 {
        let shown = tabs
            .iter()
            .filter(|t| !t.pinned && (t.active || !t.group.is_some_and(|g| self.group_collapsed(g))))
            .count()
            + 1;
        let headers = self.groups.len() as f32 * (GROUP_H + ROW_GAP);
        let visible = height - PAD - BTN - 6.0 - self.list_top(tabs);
        (shown as f32 * (TAB_H + ROW_GAP) + headers - visible).max(0.0)
    }

    fn group_collapsed(&self, g: u32) -> bool {
        self.groups.iter().any(|(id, _, c)| *id == g && *c)
    }

    /// Topmost hit at (x, y) DIPs in sidebar coordinates.
    pub fn hit(&self, tabs: &[TabView], height: f32, x: f32, y: f32) -> Option<Hit> {
        let (top, bottom, _) = self.list_area(tabs, height);
        self.layout(tabs, height)
            .into_iter()
            .rev()
            .filter(|(_, h)| !matches!(h, Hit::Tab(_) | Hit::CloseTab(_) | Hit::Group(_)) || (y >= top && y < bottom))
            .find(|(r, _)| contains(r, x, y))
            .map(|(_, h)| h)
    }

    // ---------- painting: sidebar ----------

    pub fn draw_chrome(&mut self, dc: &ID2D1DeviceContext, tabs: &[TabView], url: &str, icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, height: f32) {
        let th = self.theme;
        let floating = !self.visible;
        // A floating (peeked) sidebar gets an opaque panel over the page;
        // the solid frame itself lives in its own layer below the page.
        let p = self.peek.value();
        if self.compact {
            return self.draw_compact_bar(dc, tabs, url, icons, height, p);
        }
        if floating && p <= 0.001 {
            return;
        }
        let x0 = self.offset();
        unsafe {
            let base = windows_numerics::Matrix3x2::translation(x0, 0.0);
            let mut old = windows_numerics::Matrix3x2::default();
            dc.GetTransform(&mut old);
            dc.SetTransform(&(base * old));
            if floating {
                // Floating panel over the page: rounded, with a soft shadow.
                let panel = rect(6.0, 6.0, self.width - 2.0, height - 6.0);
                for i in 1..=5 {
                    let g = i as f32 * 2.5;
                    if let Ok(sh) = dc.CreateSolidColorBrush(&rgba(0x000000, 0.05), None) {
                        dc.FillRoundedRectangle(&rr(rect(panel.left, panel.top + g * 0.4, panel.right + g, panel.bottom + g * 0.6), 16.0 + g), &sh);
                    }
                }
                if let (Ok(b), Ok(line)) = (
                    dc.CreateSolidColorBrush(&rgba(th.frame(), 0.98), None),
                    dc.CreateSolidColorBrush(&rgba(th.ink(), 0.08), None),
                ) {
                    dc.FillRoundedRectangle(&rr(panel, 16.0), &b);
                    dc.DrawRoundedRectangle(&rr(panel, 16.0), &line, 1.0, None);
                }
            }
            self.draw_sidebar(dc, tabs, url, icons, height);
            if self.edge_hover && self.visible
                && let Ok(grip) = dc.CreateSolidColorBrush(&rgba(th.ink(), 0.35), None)
            {
                let cy = height / 2.0;
                let x = self.width - 3.0;
                dc.FillRoundedRectangle(&rr(rect(x - 1.5, cy - 22.0, x + 1.5, cy + 22.0), 1.5), &grip);
            }
            dc.SetTransform(&old);
        }
    }

    /// Compact mode: ONE bar whose width follows `p` (0 = icon strip,
    /// 1 = full sidebar over the page). Tab icons, "new tab" and settings sit
    /// at the same place in both states, so the bar just grows: its
    /// background widens, labels are revealed by its edge, and the
    /// strip-only icons cross-fade with the full sidebar's extras.
    #[allow(clippy::too_many_arguments)]
    fn draw_compact_bar(&mut self, dc: &ID2D1DeviceContext, tabs: &[TabView], url: &str, icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, height: f32, p: f32) {
        let th = self.theme;
        let w = COMPACT_W + (self.width - COMPACT_W) * p;
        let layer = |dc: &ID2D1DeviceContext, opacity: f32| unsafe {
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: rect(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: windows_numerics::Matrix3x2::identity(),
                opacity,
                ..Default::default()
            };
            dc.PushLayer(&params, None);
        };
        unsafe {
            if p > 0.001 {
                // The bar's background grows with it (opaque over the page),
                // with a soft shadow along its moving edge.
                for i in 1..=4 {
                    let g = i as f32 * 3.0;
                    if let Ok(sh) = dc.CreateSolidColorBrush(&rgba(0x000000, 0.06 * p), None) {
                        dc.FillRectangle(&rect(0.0, 0.0, w + g, height), &sh);
                    }
                }
                if let (Ok(b), Ok(line)) = (
                    dc.CreateSolidColorBrush(&rgba(th.panel(), 0.99 * (p * 3.0).min(1.0)), None),
                    dc.CreateSolidColorBrush(&rgba(th.ink(), 0.08 * p), None),
                ) {
                    dc.FillRectangle(&rect(0.0, 0.0, w, height), &b);
                    dc.FillRectangle(&rect(w - 1.0, 0.0, w, height), &line);
                }
            }
            dc.PushAxisAlignedClip(&rect(0.0, 0.0, w, height), D2D1_ANTIALIAS_MODE_ALIASED);
        }
        // Shared icons are drawn by both; the opacities overlap so they
        // never dim in the middle of the movement.
        if p < 0.999 {
            layer(dc, 1.0 - p);
            self.draw_compact(dc, tabs, icons, height);
            unsafe { dc.PopLayer() };
        }
        if p > 0.001 {
            layer(dc, (p * 2.0).min(1.0));
            self.draw_sidebar(dc, tabs, url, icons, height);
            unsafe { dc.PopLayer() };
        }
        unsafe { dc.PopAxisAlignedClip() };
    }

    /// Compact sidebar: the full sidebar's own layout reduced to its icon
    /// column, so when it unfolds on hover nothing moves: the strip just
    /// grows and the titles appear. Hovering unfolds it, so this strip is
    /// only drawn, never clicked.
    fn draw_compact(&mut self, dc: &ID2D1DeviceContext, tabs: &[TabView], icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, height: f32) {
        let th = self.theme;
        let b = |c: u32, a: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, a), None).ok() };
        let (Some(fg2), Some(fg3), Some(sel), Some(tile)) = (b(th.fg(), 0.62), b(th.fg(), 0.40), b(th.ink(), 0.12), b(th.ink(), 0.07))
        else {
            return;
        };
        let rt: &ID2D1RenderTarget = dc;
        const CELL: f32 = 34.0;
        let x = (COMPACT_W - CELL) / 2.0;
        let layout = self.layout(tabs, height);
        let (list_top, list_bottom, _) = self.list_area(tabs, height);

        // Address pill -> search icon. Bookmarks and pinned tiles: those of
        // the first column, drawn exactly where they are once the bar
        // unfolds, so what is under the pointer never changes (a pinned
        // icon used to sit where a bookmark appears).
        for (r, hit) in &layout {
            match hit {
                Hit::Url => draw_icon(&self.gfx, rt, Icon::Search, x + 9.0, (r.top + r.bottom) / 2.0 - 8.0, &fg3),
                Hit::Bookmark(i) if r.left < COMPACT_W - 20.0 => {
                    let Some((title, url)) = self.bookmarks.get(*i).cloned() else { continue };
                    unsafe { dc.FillEllipse(&circle(*r), &tile) };
                    let view = TabView { id: 0, title: &title, url: &url, pinned: false, active: false, unloaded: false, audio: false, group: None, icon: icons.get(&crate::app::host_of(&url)) };
                    self.draw_favicon(dc, &view, r.left + 7.0, r.top + 7.0, 16.0, &fg2);
                }
                Hit::Tile(id) if r.left < COMPACT_W - 20.0 => {
                    let Some(t) = tabs.iter().find(|t| t.id == *id) else { continue };
                    let y = (r.top + r.bottom) / 2.0 - CELL / 2.0;
                    unsafe { dc.FillRoundedRectangle(&rr(rect(x, y, x + CELL, y + CELL), 10.0), if t.active { &sel } else { &tile }) };
                    self.draw_favicon(dc, t, x + 9.0, y + 9.0, 16.0, &fg2);
                }
                _ => {}
            }
        }
        unsafe { dc.PushAxisAlignedClip(&rect(0.0, list_top, COMPACT_W, list_bottom), D2D1_ANTIALIAS_MODE_ALIASED) };
        for (r, hit) in &layout {
            if let Hit::Tab(id) = hit
                && let Some(t) = tabs.iter().find(|t| t.id == *id)
            {
                if t.active {
                    unsafe { dc.FillRoundedRectangle(&rr(rect(x, r.top, x + CELL, r.top + CELL), 10.0), &sel) };
                }
                self.draw_favicon(dc, t, r.left + 9.0, r.top + 9.0, 16.0, &fg2);
            }
        }
        unsafe { dc.PopAxisAlignedClip() };
        for (r, hit) in &layout {
            match hit {
                Hit::NewTab => draw_icon(&self.gfx, rt, Icon::Plus, r.left + 9.0, r.top + 9.0, &fg3),
                Hit::Settings => draw_icon(&self.gfx, rt, Icon::Gear, r.left + (BTN - 16.0) / 2.0, r.top + (BTN - 16.0) / 2.0, &fg2),
                _ => {}
            }
        }
    }

    fn draw_sidebar(&mut self, dc: &ID2D1DeviceContext, tabs: &[TabView], url: &str, icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, height: f32) {
        let th = self.theme;
        let b = |c: u32, a: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, a), None).ok() };
        let (Some(fg), Some(fg2), Some(fg3)) = (b(th.fg(), 1.0), b(th.fg(), 0.62), b(th.fg(), 0.40)) else { return };
        let rt: &ID2D1RenderTarget = dc;
        let ink = th.ink();

        // Selection highlight slides between rows.
        let active_row = self.layout(tabs, height).into_iter().find_map(|(r, h)| match h {
            Hit::Tab(id) if tabs.iter().any(|t| t.id == id && t.active) => Some((id, r.top)),
            _ => None,
        });
        match active_row {
            Some((id, top)) if self.sel_id != Some(id) || (self.sel_y.target() - top).abs() > 0.5 => {
                if self.sel_id.is_none() || self.drag.as_ref().is_some_and(|d| d.moving) {
                    self.sel_y.set(top);
                } else {
                    self.sel_y.go(top, self.ms(180));
                }
                self.sel_id = Some(id);
            }
            None => self.sel_id = None,
            _ => {}
        }

        let (list_top, list_bottom, _) = self.list_area(tabs, height);
        let mut clipped = false;
        for (r, hit) in self.layout(tabs, height) {
            let h = self.hover_amount(hit);
            if clipped {
                unsafe { dc.PopAxisAlignedClip() };
                clipped = false;
            }
            // Rows scrolled partly out of the list are cut at its edges.
            if matches!(hit, Hit::Tab(_) | Hit::CloseTab(_) | Hit::Group(_)) && (r.top < list_top || r.bottom > list_bottom) {
                unsafe { dc.PushAxisAlignedClip(&rect(0.0, list_top, self.width, list_bottom), D2D1_ANTIALIAS_MODE_ALIASED) };
                clipped = true;
            }
            unsafe {
                match hit {
                    Hit::Caption => {}
                    Hit::Back | Hit::Forward | Hit::Reload | Hit::Settings | Hit::Downloads | Hit::Minimize | Hit::Maximize | Hit::Close => {
                        if h > 0.01 {
                            let (c, a) = if hit == Hit::Close { (0xE5484D, 0.9 * h) } else { (ink, 0.08 * h) };
                            if let Some(hb) = b(c, a) {
                                dc.FillEllipse(&circle(r), &hb);
                            }
                        }
                        let icon = match hit {
                            Hit::Back => Icon::Back,
                            Hit::Forward => Icon::Forward,
                            Hit::Reload => Icon::Reload,
                            Hit::Settings => Icon::Gear,
                            Hit::Downloads => Icon::Download,
                            Hit::Minimize => Icon::Minimize,
                            Hit::Maximize if self.maximized => Icon::Restore,
                            Hit::Maximize => Icon::Maximize,
                            _ => Icon::Close,
                        };
                        let color = if hit == Hit::Close && h > 0.5 { b(0xFFFFFF, 1.0) } else { Some(fg2.clone()) };
                        if let Some(c) = color {
                            draw_icon(&self.gfx, rt, icon, r.left + (BTN - 16.0) / 2.0, r.top + (BTN - 16.0) / 2.0, &c);
                        }
                    }
                    Hit::Url => {
                        if let Some(bg) = b(ink, 0.06 + 0.04 * h) {
                            dc.FillRoundedRectangle(&rr(r, 10.0), &bg);
                        }
                        let text: Vec<u16> = if url.is_empty() { t("Cerca o scrivi un indirizzo", "Search or enter address").encode_utf16().collect() } else { url.encode_utf16().collect() };
                        let pad = if self.has_page { 58.0 } else { 12.0 };
                        let tr = rect(r.left + pad, r.top, r.right - pad, r.bottom);
                        let c = if url.is_empty() { &fg3 } else { &fg2 };
                        dc.DrawText(&text, &self.fonts.url, &tr, c, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                    }
                    Hit::Group(g) => {
                        let Some((_, name, collapsed)) = self.groups.iter().find(|(id, ..)| *id == g).cloned() else { continue };
                        if h > 0.01
                            && let Some(bg) = b(ink, 0.06 * h)
                        {
                            dc.FillRoundedRectangle(&rr(r, 9.0), &bg);
                        }
                        let icon = if collapsed { Icon::Forward } else { Icon::ChevronDown };
                        draw_icon(&self.gfx, rt, icon, r.left + 7.0, r.top + 7.0, &fg3);
                        let count = tabs.iter().filter(|t| t.group == Some(g)).count();
                        let label = if collapsed { format!("{name}  \u{00B7}  {count}") } else { name };
                        let text: Vec<u16> = label.encode_utf16().collect();
                        dc.DrawText(&text, &self.fonts.title_bold, &rect(r.left + 30.0, r.top, r.right - 10.0, r.bottom), &fg2, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                    }
                    Hit::Extensions => {
                        if h > 0.01
                            && let Some(bg) = b(ink, 0.08 * h)
                        {
                            dc.FillEllipse(&circle(r), &bg);
                        }
                        draw_icon(&self.gfx, rt, Icon::Puzzle, r.left + (r.right - r.left - 16.0) / 2.0, r.top + (r.bottom - r.top - 16.0) / 2.0, &fg3);
                    }
                    Hit::Star => {
                        if h > 0.01
                            && let Some(bg) = b(ink, 0.08 * h)
                        {
                            dc.FillEllipse(&circle(r), &bg);
                        }
                        let icon = if self.bookmarked { Icon::StarFilled } else { Icon::Star };
                        let color = if self.bookmarked { fg.clone() } else { fg3.clone() };
                        draw_icon(&self.gfx, rt, icon, r.left + (r.right - r.left - 16.0) / 2.0, r.top + (r.bottom - r.top - 16.0) / 2.0, &color);
                    }
                    Hit::Bookmark(i) => {
                        let Some((title, url)) = self.bookmarks.get(i).cloned() else { continue };
                        if let Some(bg) = b(ink, 0.07 + 0.06 * h) {
                            dc.FillEllipse(&circle(r), &bg);
                        }
                        let view = TabView { id: 0, title: &title, url: &url, pinned: false, active: false, unloaded: false, audio: false, group: None, icon: icons.get(&crate::app::host_of(&url)) };
                        self.draw_favicon(dc, &view, r.left + 7.0, r.top + 7.0, 16.0, &fg2);
                    }
                    Hit::MoreBookmarks => {
                        if let Some(bg) = b(ink, 0.07 + 0.06 * h) {
                            dc.FillEllipse(&circle(r), &bg);
                        }
                        draw_icon(&self.gfx, rt, Icon::ChevronDown, r.left + 7.0, r.top + 7.5, &fg2);
                    }
                    Hit::Tile(id) => {
                        let Some(t) = tabs.iter().find(|t| t.id == id) else { continue };
                        let a = if t.active { 0.16 } else { 0.07 + 0.05 * h };
                        if let Some(bg) = b(ink, a) {
                            dc.FillRoundedRectangle(&rr(r, 12.0), &bg);
                        }
                        let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
                        self.draw_favicon(dc, t, cx - 9.0, cy - 9.0, 18.0, &fg2);
                    }
                    Hit::Tab(id) => {
                        let Some(t) = tabs.iter().find(|t| t.id == id) else { continue };
                        let dragging = self.drag.as_ref().is_some_and(|d| d.moving && d.id == id);
                        if t.active {
                            let y = if dragging { r.top } else { self.sel_y.value() };
                            if let Some(bg) = b(ink, 0.12) {
                                dc.FillRoundedRectangle(&rr(rect(r.left, y, r.right, y + TAB_H), 10.0), &bg);
                            }
                        }
                        let hh = h.max(self.hover_amount(Hit::CloseTab(id)));
                        if !t.active && hh > 0.01
                            && let Some(bg) = b(ink, 0.06 * hh)
                        {
                            dc.FillRoundedRectangle(&rr(r, 10.0), &bg);
                        }
                        if dragging && let Some(bg) = b(ink, 0.10) {
                            dc.FillRoundedRectangle(&rr(r, 10.0), &bg);
                        }
                        self.draw_favicon(dc, t, r.left + 9.0, r.top + 9.0, 16.0, &fg2);
                        let title: Vec<u16> = tab_label(t).encode_utf16().collect();
                        let font = if t.active { &self.fonts.title_bold } else { &self.fonts.title };
                        let show_close = t.active || hh > 0.01;
                        let right = r.right - if show_close { 30.0 } else { 10.0 } - if t.audio { 18.0 } else { 0.0 };
                        let color = if t.unloaded && !t.active { &fg2 } else { &fg };
                        dc.DrawText(&title, font, &rect(r.left + 34.0, r.top, right, r.bottom), color, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                        if t.audio {
                            draw_icon(&self.gfx, rt, Icon::Audio, right + 1.0, r.top + 9.0, &fg2);
                        }
                    }
                    Hit::CloseTab(id) => {
                        let row_h = self.hover_amount(Hit::Tab(id));
                        let active = tabs.iter().any(|t| t.id == id && t.active);
                        let vis = if active { 1.0 } else { row_h.max(h) };
                        if vis > 0.01 {
                            if h > 0.01
                                && let Some(bg) = b(ink, 0.10 * h)
                            {
                                dc.FillRoundedRectangle(&rr(r, 7.0), &bg);
                            }
                            if let Some(c) = b(th.fg(), 0.62 * vis + 0.38 * h) {
                                draw_icon(&self.gfx, rt, Icon::Close, r.left + 4.0, r.top + 5.0, &c);
                            }
                        }
                    }
                    Hit::NewTab => {
                        if h > 0.01
                            && let Some(bg) = b(ink, 0.06 * h)
                        {
                            dc.FillRoundedRectangle(&rr(r, 10.0), &bg);
                        }
                        draw_icon(&self.gfx, rt, Icon::Plus, r.left + 9.0, r.top + 9.0, &fg3);
                        let text: Vec<u16> = t("Nuova scheda", "New tab").encode_utf16().collect();
                        dc.DrawText(&text, &self.fonts.title, &rect(r.left + 34.0, r.top, r.right - 10.0, r.bottom), &fg3, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
            }
        }
        if clipped {
            unsafe { dc.PopAxisAlignedClip() };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_favicon(&mut self, dc: &ID2D1DeviceContext, t: &TabView, x: f32, y: f32, size: f32, fallback: &ID2D1SolidColorBrush) {
        let Some((color, gray)) = t.icon else {
            // No favicon: the site's initial on a soft tile, so tabs stay
            // tell-apart (above all in the compact sidebar).
            let host = crate::app::host_of(t.url);
            let host = host.strip_prefix("www.").unwrap_or(&host);
            let letter = host.chars().next().filter(|c| c.is_alphanumeric() && t.url.starts_with("http") && !t.url.starts_with(crate::app::SETTINGS_URL));
            match letter {
                Some(c) => unsafe {
                    let a = if t.unloaded { 0.06 } else { 0.12 };
                    if let Ok(bg) = dc.CreateSolidColorBrush(&rgba(self.theme.ink(), a), None) {
                        dc.FillRoundedRectangle(&rr(rect(x, y, x + size, y + size), size * 0.28), &bg);
                    }
                    let text: Vec<u16> = c.to_uppercase().to_string().encode_utf16().collect();
                    dc.DrawText(&text, &self.fonts.chip, &rect(x, y, x + size, y + size), fallback, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                },
                None => {
                    let icon = if t.url.starts_with(crate::app::SETTINGS_URL) { Icon::Gear } else { Icon::Globe };
                    draw_icon(&self.gfx, dc, icon, x + (size - 16.0) / 2.0, y + (size - 16.0) / 2.0, fallback);
                }
            }
            return;
        };
        // Unloaded tabs show their icon in grayscale (user request 4.5).
        let src = if t.unloaded { gray } else { color };
        let key = (src.as_raw() as usize, t.unloaded);
        if !self.bitmaps.contains_key(&key)
            && let Ok(bmp) = unsafe { dc.CreateBitmapFromWicBitmap(src, None) }
        {
            self.bitmaps.insert(key, bmp.into());
        }
        if let Some(bmp) = self.bitmaps.get(&key) {
            let opacity = if t.unloaded { 0.55 } else { 1.0 };
            unsafe {
                dc.DrawBitmap(bmp, Some(&rect(x, y, x + size, y + size)), opacity, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            }
        }
    }

    // ---------- overlay: command bar, tooltip ----------

    /// Command bar panel rect for the page area (DIPs).
    pub fn bar_rect(&self, page: D2D_RECT_F) -> D2D_RECT_F {
        let pw = page.right - page.left;
        let w = (pw - 48.0).clamp(320.0, 680.0);
        let x = page.left + (pw - w) / 2.0;
        let y = page.top + ((page.bottom - page.top) * 0.16).clamp(48.0, 140.0);
        let rows = self.bar.items.len().min(8) as f32;
        let h = 56.0 + if rows > 0.0 { 8.0 + rows * 44.0 + 8.0 } else { 0.0 };
        rect(x, y, x + w, y + h)
    }

    pub fn bar_row_at(&self, page: D2D_RECT_F, x: f32, y: f32) -> Option<usize> {
        let r = self.bar_rect(page);
        if !contains(&r, x, y) {
            return None;
        }
        let top = r.top + 56.0 + 8.0;
        (y >= top).then(|| ((y - top) / 44.0) as usize).filter(|i| *i < self.bar.items.len().min(8))
    }

    pub fn in_bar(&self, page: D2D_RECT_F, x: f32, y: f32) -> bool {
        contains(&self.bar_rect(page), x, y)
    }

    /// Caret index for a click at x (DIPs) inside the command bar field.
    pub fn field_index_at(&self, page: D2D_RECT_F, x: f32) -> usize {
        let r = self.bar_rect(page);
        let local = x - (r.left + 48.0);
        let text = &self.bar.field.text;
        unsafe {
            let Ok(layout) = self.gfx.dwrite.CreateTextLayout(text, &self.fonts.field, 10_000.0, 56.0) else { return text.len() };
            let (mut trailing, mut inside) = (Default::default(), Default::default());
            let mut m = DWRITE_HIT_TEST_METRICS::default();
            if layout.HitTestPoint(local, 20.0, &mut trailing, &mut inside, &mut m).is_ok() {
                let i = m.textPosition as usize + if trailing.as_bool() { m.length as usize } else { 0 };
                return i.min(text.len());
            }
        }
        text.len()
    }

    pub fn draw_overlay(&mut self, dc: &ID2D1DeviceContext, page: D2D_RECT_F, icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, keys_for_enter: &str) {
        let t = self.bar.t.value();
        if t > 0.001 {
            self.draw_bar(dc, page, t, icons, keys_for_enter);
        }
        if let Some(tip) = &self.tip {
            let a = tip.t.value();
            if a > 0.001 {
                self.draw_tip(dc, tip, a);
            }
        }
        if let Some(menu) = &self.menu {
            self.draw_menu(dc, menu);
        }
    }

    /// Dims the page around a card (link preview, history panel); the card
    /// itself stays undimmed. Follows both rounded outlines (square bands
    /// showed past the page's rounded corners). `amount`: 0..1 while fading.
    #[allow(clippy::too_many_arguments)]
    pub fn dim_around(&self, dc: &ID2D1DeviceContext, page: D2D_RECT_F, page_radius: f32, card: D2D_RECT_F, card_radius: f32, amount: f32) {
        unsafe {
            let Ok(dim) = dc.CreateSolidColorBrush(&rgba(0x000000, amount * if self.theme.dark { 0.32 } else { 0.18 }), None) else { return };
            let shape = dc.GetFactory().ok().and_then(|f| {
                let outer = f.CreateRoundedRectangleGeometry(&rr(page, page_radius)).ok()?;
                let hole = f.CreateRoundedRectangleGeometry(&rr(card, card_radius)).ok()?;
                // Alternate fill: inside the page, outside the card.
                f.CreateGeometryGroup(D2D1_FILL_MODE_ALTERNATE, &[Some(outer.cast().ok()?), Some(hole.cast().ok()?)]).ok()
            });
            if let Some(shape) = shape {
                dc.FillGeometry(&shape, &dim, None);
            }
        }
    }

    /// Around the preview card: dim the rest and draw the two buttons.
    pub fn draw_peek_chrome(&self, dc: &ID2D1DeviceContext, page: D2D_RECT_F, card: D2D_RECT_F, buttons: (D2D_RECT_F, D2D_RECT_F), hover: Option<crate::peek::PeekButton>) {
        use crate::peek::PeekButton;
        let th = self.theme;
        self.dim_around(dc, page, PAGE_RADIUS, card, PAGE_RADIUS, 1.0);
        unsafe {
            let (open, close) = buttons;
            if let (Ok(bg), Ok(bgh), Ok(fg)) = (
                dc.CreateSolidColorBrush(&rgba(th.panel(), 0.96), None),
                dc.CreateSolidColorBrush(&rgba(th.panel(), 1.0), None),
                dc.CreateSolidColorBrush(&rgba(th.fg(), 0.9), None),
            ) {
                let pick = |b: PeekButton| if hover == Some(b) { &bgh } else { &bg };
                dc.FillRoundedRectangle(&rr(open, 16.0), pick(PeekButton::OpenTab));
                let label: Vec<u16> = t("Apri in una scheda", "Open as tab").encode_utf16().collect();
                dc.DrawText(&label, &self.fonts.chip, &open, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                dc.FillEllipse(&circle(close), pick(PeekButton::Close));
                draw_icon(&self.gfx, dc, Icon::Close, close.left + 8.0, close.top + 8.0, &fg);
            }
        }
    }

    /// Header strip above the docked DevTools: title and a close button.
    pub fn draw_dock_header(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, close_hover: bool) {
        let th = self.theme;
        unsafe {
            if let (Ok(bg), Ok(fg), Ok(fg2)) = (
                dc.CreateSolidColorBrush(&rgba(th.panel(), 1.0), None),
                dc.CreateSolidColorBrush(&rgba(th.fg(), 0.85), None),
                dc.CreateSolidColorBrush(&rgba(th.fg(), 0.6), None),
            ) {
                // Rounded top corners only (the DevTools window covers the rest).
                dc.FillRoundedRectangle(&rr(rect(r.left, r.top, r.right, r.bottom + 12.0), PAGE_RADIUS), &bg);
                let title: Vec<u16> = t("Strumenti per sviluppatori", "Developer tools").encode_utf16().collect();
                dc.DrawText(&title, &self.fonts.small, &rect(r.left + 14.0, r.top, r.right - 44.0, r.bottom), &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                let b = self.dock_close_rect(r);
                if close_hover && let Ok(h) = dc.CreateSolidColorBrush(&rgba(th.ink(), 0.10), None) {
                    dc.FillEllipse(&circle(b), &h);
                }
                draw_icon(&self.gfx, dc, Icon::Close, b.left + 6.0, b.top + 6.0, &fg2);
            }
        }
    }

    /// Download card (bottom left of the page): name, status or progress,
    /// buttons; a flagged file gets a warning line.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_download_card(
        &self,
        dc: &ID2D1DeviceContext,
        card: D2D_RECT_F,
        name: &str,
        status: &str,
        progress: Option<f32>,
        flagged: bool,
        buttons: &[(D2D_RECT_F, crate::downloads::CardButton, &str)],
        hover: Option<crate::downloads::CardButton>,
        height: f32,
    ) {
        let th = self.theme;
        // Leaving: shrink and fade on the way to the downloads button.
        let out = self.card_out.value();
        let mut old = windows_numerics::Matrix3x2::default();
        unsafe { dc.GetTransform(&mut old) };
        if out > 0.0 {
            let (cx, cy) = ((card.left + card.right) / 2.0, (card.top + card.bottom) / 2.0);
            let (tx, ty) = (PAD - 2.0 + BTN * 1.5 + 4.0, height - PAD - BTN / 2.0);
            let s = 1.0 - 0.9 * out;
            let (dx, dy) = ((tx - cx) * out, (ty - cy) * out);
            let m = windows_numerics::Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: cx * (1.0 - s) + dx, M32: cy * (1.0 - s) + dy };
            unsafe {
                dc.SetTransform(&(m * old));
                let params = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: rect(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows_numerics::Matrix3x2::identity(),
                    opacity: (1.0 - out * out).max(0.0),
                    ..Default::default()
                };
                dc.PushLayer(&params, None);
            }
        }
        self.draw_card_body(dc, card, name, status, progress, flagged, buttons, hover, th);
        if out > 0.0 {
            unsafe {
                dc.PopLayer();
                dc.SetTransform(&old);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_card_body(
        &self,
        dc: &ID2D1DeviceContext,
        card: D2D_RECT_F,
        name: &str,
        status: &str,
        progress: Option<f32>,
        flagged: bool,
        buttons: &[(D2D_RECT_F, crate::downloads::CardButton, &str)],
        hover: Option<crate::downloads::CardButton>,
        th: Theme,
    ) {
        use crate::downloads::CardButton;
        let b = |c: u32, al: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, al), None).ok() };
        let text = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        unsafe {
            for i in 1..=4 {
                let g = i as f32 * 3.0;
                if let Some(sh) = b(0x000000, 0.05) {
                    dc.FillRoundedRectangle(&rr(rect(card.left - g * 0.4, card.top + g * 0.5, card.right + g * 0.4, card.bottom + g), 16.0 + g), &sh);
                }
            }
            let (Some(bg), Some(line), Some(fg), Some(fg2), Some(soft), Some(hov), Some(track), Some(bar), Some(warn)) = (
                b(th.panel(), 0.98),
                b(th.ink(), 0.09),
                b(th.fg(), 1.0),
                b(th.fg(), 0.66),
                b(th.ink(), 0.07),
                b(th.ink(), 0.12),
                b(th.ink(), 0.10),
                b(th.fg(), 0.85),
                b(0xE5484D, 1.0),
            ) else {
                return;
            };
            dc.FillRoundedRectangle(&rr(card, 16.0), &bg);
            dc.DrawRoundedRectangle(&rr(card, 16.0), &line, 1.0, None);
            draw_icon(&self.gfx, dc, Icon::Download, card.left + 16.0, card.top + 16.0, if flagged { &warn } else { &fg2 });
            dc.DrawText(&text(name), &self.fonts.title_bold, &rect(card.left + 42.0, card.top + 12.0, card.right - 44.0, card.top + 36.0), &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            let line2 = if flagged { t("Da un sito segnalato come pericoloso", "From a site reported as dangerous").to_string() } else { status.to_string() };
            dc.DrawText(&text(&line2), &self.fonts.small, &rect(card.left + 42.0, card.top + 34.0, card.right - 16.0, card.top + 54.0), if flagged { &warn } else { &fg2 }, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            if let Some(p) = progress {
                let r = rect(card.left + 16.0, card.top + 60.0, card.left + 16.0 + (card.right - card.left - 32.0) * 0.55, card.top + 64.0);
                dc.FillRoundedRectangle(&rr(r, 2.0), &track);
                dc.FillRoundedRectangle(&rr(rect(r.left, r.top, r.left + (r.right - r.left) * p, r.bottom), 2.0), &bar);
            }
            for (r, which, label) in buttons {
                let on = hover == Some(*which);
                if *which == CardButton::Close {
                    if on {
                        dc.FillEllipse(&circle(*r), &hov);
                    }
                    draw_icon(&self.gfx, dc, Icon::Close, r.left + 5.0, r.top + 5.0, &fg2);
                    continue;
                }
                let danger = *which == CardButton::Delete;
                dc.FillRoundedRectangle(&rr(*r, 9.0), if on { &hov } else { &soft });
                dc.DrawText(&text(label), &self.fonts.url, r, if danger && on { &warn } else { &fg }, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
        }
    }

    /// Short message pill at the bottom center of the page.
    pub fn draw_notice(&self, dc: &ID2D1DeviceContext, page: D2D_RECT_F, text: &str) {
        let th = self.theme;
        let w = (text.chars().count() as f32 * 7.0 + 40.0).min(page.right - page.left - 40.0);
        let cx = (page.left + page.right) / 2.0;
        let r = rect(cx - w / 2.0, page.bottom - 64.0, cx + w / 2.0, page.bottom - 24.0);
        let label: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            for i in 1..=3 {
                let g = i as f32 * 3.0;
                if let Ok(sh) = dc.CreateSolidColorBrush(&rgba(0x000000, 0.06), None) {
                    dc.FillRoundedRectangle(&rr(rect(r.left - g * 0.4, r.top + g * 0.5, r.right + g * 0.4, r.bottom + g), 20.0 + g), &sh);
                }
            }
            if let (Ok(bg), Ok(line), Ok(fg)) = (
                dc.CreateSolidColorBrush(&rgba(th.panel(), 0.98), None),
                dc.CreateSolidColorBrush(&rgba(th.ink(), 0.09), None),
                dc.CreateSolidColorBrush(&rgba(th.fg(), 1.0), None),
            ) {
                dc.FillRoundedRectangle(&rr(r, 20.0), &bg);
                dc.DrawRoundedRectangle(&rr(r, 20.0), &line, 1.0, None);
                dc.DrawText(&label, &self.fonts.url, &rect(r.left + 16.0, r.top, r.right - 16.0, r.bottom), &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            }
        }
    }

    /// Site permission card: "<site> wants to <what>", Block / Allow, X.
    pub fn draw_prompt(&self, dc: &ID2D1DeviceContext, card: D2D_RECT_F, host: &str, what: &str, hover: Option<crate::permission::Choice>) {
        use crate::permission::Choice;
        let th = self.theme;
        let b = |c: u32, al: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, al), None).ok() };
        let text = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        unsafe {
            for i in 1..=4 {
                let g = i as f32 * 3.0;
                if let Some(sh) = b(0x000000, 0.05) {
                    dc.FillRoundedRectangle(&rr(rect(card.left - g * 0.4, card.top + g * 0.5, card.right + g * 0.4, card.bottom + g), 16.0 + g), &sh);
                }
            }
            let (Some(bg), Some(line), Some(fg), Some(fg2), Some(soft), Some(hov), Some(strong), Some(on_strong)) = (
                b(th.panel(), 0.98),
                b(th.ink(), 0.09),
                b(th.fg(), 1.0),
                b(th.fg(), 0.66),
                b(th.ink(), 0.07),
                b(th.ink(), 0.12),
                b(th.fg(), 0.92),
                b(th.panel(), 1.0),
            ) else {
                return;
            };
            dc.FillRoundedRectangle(&rr(card, 16.0), &bg);
            dc.DrawRoundedRectangle(&rr(card, 16.0), &line, 1.0, None);
            dc.DrawText(&text(host), &self.fonts.title_bold, &rect(card.left + 18.0, card.top + 14.0, card.right - 48.0, card.top + 36.0), &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            let line2 = format!("{} {}", t("vuole", "wants to"), what);
            dc.DrawText(&text(&line2), &self.fonts.title, &rect(card.left + 18.0, card.top + 34.0, card.right - 18.0, card.top + 56.0), &fg2, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            for (r, c) in crate::app::App::prompt_buttons(card) {
                let on = hover == Some(c);
                match c {
                    Choice::Dismiss => {
                        if on {
                            dc.FillEllipse(&circle(r), &hov);
                        }
                        draw_icon(&self.gfx, dc, Icon::Close, r.left + 6.0, r.top + 6.0, &fg2);
                    }
                    Choice::Block | Choice::Allow => {
                        let primary = c == Choice::Allow;
                        let fill = if primary { &strong } else if on { &hov } else { &soft };
                        dc.FillRoundedRectangle(&rr(r, 10.0), fill);
                        if primary && on {
                            dc.FillRoundedRectangle(&rr(r, 10.0), &hov);
                        }
                        let label = if primary { t("Consenti", "Allow") } else { t("Blocca", "Block") };
                        dc.DrawText(&text(label), &self.fonts.url, &r, if primary { &on_strong } else { &fg }, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                    }
                }
            }
        }
    }

    pub fn dock_close_rect(&self, header: D2D_RECT_F) -> D2D_RECT_F {
        let cy = (header.top + header.bottom) / 2.0;
        rect(header.right - 36.0, cy - 14.0, header.right - 8.0, cy + 14.0)
    }

    /// Menu panel rect, kept inside the window (`w`×`h` DIPs).
    pub fn menu_rect(&self, m: &Menu, w: f32, h: f32) -> D2D_RECT_F {
        let seps = m.items.iter().filter(|i| i.sep).count() as f32;
        let height = 12.0 + m.items.len() as f32 * MENU_ROW + seps * 9.0;
        let x = m.x.min(w - MENU_W - 8.0).max(8.0);
        let y = if m.y + height > h - 8.0 { (m.y - height).max(8.0) } else { m.y };
        rect(x, y, x + MENU_W, y + height)
    }

    /// Row tops of the menu items (separators add space).
    fn menu_rows(&self, m: &Menu, r: D2D_RECT_F) -> Vec<f32> {
        let mut y = r.top + 6.0;
        m.items
            .iter()
            .map(|it| {
                if it.sep {
                    y += 9.0;
                }
                let top = y;
                y += MENU_ROW;
                top
            })
            .collect()
    }

    pub fn menu_item_at(&self, w: f32, h: f32, x: f32, y: f32) -> Option<usize> {
        let m = self.menu.as_ref()?;
        let r = self.menu_rect(m, w, h);
        if !contains(&r, x, y) {
            return None;
        }
        self.menu_rows(m, r).iter().position(|top| y >= *top && y < top + MENU_ROW)
    }

    pub fn in_menu(&self, w: f32, h: f32, x: f32, y: f32) -> bool {
        self.menu.as_ref().is_some_and(|m| contains(&self.menu_rect(m, w, h), x, y))
    }

    fn draw_menu(&self, dc: &ID2D1DeviceContext, m: &Menu) {
        let a = m.t.value();
        if a <= 0.001 {
            return;
        }
        let th = self.theme;
        let size = unsafe { dc.GetSize() };
        let r = self.menu_rect(m, size.width, size.height);
        let b = |c: u32, al: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, al * a), None).ok() };
        unsafe {
            for i in 1..=4 {
                let g = i as f32 * 3.0;
                if let Some(sh) = b(0x000000, 0.04) {
                    dc.FillRoundedRectangle(&rr(rect(r.left - g * 0.4, r.top + g * 0.5, r.right + g * 0.4, r.bottom + g), 14.0 + g), &sh);
                }
            }
            if let (Some(bg), Some(line)) = (b(th.panel(), 0.98), b(th.ink(), 0.09)) {
                dc.FillRoundedRectangle(&rr(r, 14.0), &bg);
                dc.DrawRoundedRectangle(&rr(r, 14.0), &line, 1.0, None);
            }
            let (Some(fg), Some(fg3), Some(hov), Some(line), Some(red)) =
                (b(th.fg(), 1.0), b(th.fg(), 0.42), b(th.ink(), 0.08), b(th.ink(), 0.08), b(0xE5484D, 1.0))
            else {
                return;
            };
            for (i, (it, top)) in m.items.iter().zip(self.menu_rows(m, r)).enumerate() {
                if it.sep {
                    dc.FillRectangle(&rect(r.left + 12.0, top - 5.0, r.right - 12.0, top - 4.0), &line);
                }
                let row = rect(r.left + 6.0, top, r.right - 6.0, top + MENU_ROW);
                if m.hover == Some(i) {
                    dc.FillRoundedRectangle(&rr(row, 9.0), &hov);
                }
                let destructive = matches!(it.cmd, MenuCmd::Close | MenuCmd::RemoveBookmark(_));
                let color = if destructive && m.hover == Some(i) { &red } else { &fg };
                let label: Vec<u16> = it.label.encode_utf16().collect();
                let label_right = if it.keys.is_empty() { row.right - 10.0 } else { row.right - 90.0 };
                dc.DrawText(&label, &self.fonts.title, &rect(row.left + 10.0, top, label_right, top + MENU_ROW), color, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                if !it.keys.is_empty() {
                    let keys: Vec<u16> = it.keys.encode_utf16().collect();
                    let kr = rect(row.right - 90.0, top, row.right - 10.0, top + MENU_ROW);
                    let _ = self.fonts.small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
                    dc.DrawText(&keys, &self.fonts.small, &kr, &fg3, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                    let _ = self.fonts.small.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
                }
            }
        }
    }

    fn draw_bar(&self, dc: &ID2D1DeviceContext, page: D2D_RECT_F, t: f32, icons: &HashMap<String, (IWICBitmap, IWICBitmap)>, enter_label: &str) {
        let th = self.theme;
        let b = |c: u32, a: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, a * t), None).ok() };
        unsafe {
            // Dim the page (it is also blurred by the compositor), but not
            // a sidebar floating over it (compact or auto-hide, unfolded).
            if let Some(dim) = b(0x000000, if th.dark { 0.30 } else { 0.16 }) {
                let keep = self.floating_right();
                dc.PushAxisAlignedClip(&rect(keep, page.top, page.right, page.bottom), D2D1_ANTIALIAS_MODE_ALIASED);
                dc.FillRoundedRectangle(&rr(page, PAGE_RADIUS), &dim);
                dc.PopAxisAlignedClip();
            }
            let r = self.bar_rect(page);
            let (cx, cy) = ((r.left + r.right) / 2.0, r.top);
            let s = 0.97 + 0.03 * t;
            let mut old = windows_numerics::Matrix3x2::default();
            dc.GetTransform(&mut old);
            let scale = windows_numerics::Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: cx * (1.0 - s), M32: cy * (1.0 - s) + (1.0 - t) * -6.0 };
            dc.SetTransform(&(scale * old));

            // Soft shadow: stacked translucent rounded rects.
            for i in 1..=6 {
                let g = i as f32 * 3.0;
                if let Some(sh) = b(0x000000, 0.035) {
                    dc.FillRoundedRectangle(&rr(rect(r.left - g * 0.5, r.top + g * 0.6, r.right + g * 0.5, r.bottom + g * 1.2), 18.0 + g), &sh);
                }
            }
            let rt: &ID2D1RenderTarget = dc;
            if let (Some(bg), Some(line)) = (b(th.panel(), 0.97), b(th.ink(), if th.dark { 0.10 } else { 0.08 })) {
                dc.FillRoundedRectangle(&rr(r, 18.0), &bg);
                dc.DrawRoundedRectangle(&rr(r, 18.0), &line, 1.0, None);
            }
            let (Some(fg), Some(fg2), Some(fg3), Some(sel), Some(hov)) =
                (b(th.fg(), 1.0), b(th.fg(), 0.6), b(th.fg(), 0.38), b(th.ink(), 0.09), b(th.ink(), 0.05))
            else {
                dc.SetTransform(&old);
                return;
            };

            // Input row.
            draw_icon(&self.gfx, rt, Icon::Search, r.left + 20.0, r.top + 20.0, &fg2);
            let fx = r.left + 48.0;
            let field = &self.bar.field;
            let text = &field.text;
            let layout = self.gfx.dwrite.CreateTextLayout(text, &self.fonts.field, r.right - fx - 20.0, 56.0).ok();
            let caret_x = |i: usize| -> f32 {
                let Some(l) = &layout else { return 0.0 };
                let (mut x, mut y) = (0.0, 0.0);
                let mut m = DWRITE_HIT_TEST_METRICS::default();
                let _ = l.HitTestTextPosition(i as u32, false, &mut x, &mut y, &mut m);
                x
            };
            let (a, bsel) = field.selection();
            if a != bsel
                && let Some(selb) = b(th.ink(), 0.16)
            {
                dc.FillRectangle(&rect(fx + caret_x(a), r.top + 16.0, fx + caret_x(bsel), r.top + 40.0), &selb);
            }
            if text.is_empty() {
                let ph_text = if self.bar.rename.is_some() { tr("Nome del gruppo", "Group name") } else { tr("Cerca o scrivi un indirizzo", "Search or enter address") };
                let ph: Vec<u16> = ph_text.encode_utf16().collect();
                dc.DrawText(&ph, &self.fonts.field, &rect(fx, r.top, r.right - 20.0, r.top + 56.0), &fg3, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
            } else if let Some(l) = &layout {
                // The format centers vertically within the 56 px layout box.
                dc.DrawTextLayout(windows_numerics::Vector2 { X: fx, Y: r.top }, l, &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP);
            }
            // Caret blinks (solid right after typing).
            let since = self.bar.caret_blink.elapsed().as_millis();
            if self.bar.open && (since < 500 || (since / 530) % 2 == 0) {
                let cx = fx + caret_x(field.caret);
                if let Some(c) = b(th.fg(), 0.9) {
                    dc.FillRectangle(&rect(cx, r.top + 17.0, cx + 1.5, r.top + 39.0), &c);
                }
            }

            // Results.
            if !self.bar.items.is_empty()
                && let Some(line) = b(th.ink(), 0.07)
            {
                dc.FillRectangle(&rect(r.left + 16.0, r.top + 56.0, r.right - 16.0, r.top + 57.0), &line);
            }
            for (i, item) in self.bar.items.iter().take(8).enumerate() {
                let top = r.top + 56.0 + 8.0 + i as f32 * 44.0;
                let row = rect(r.left + 8.0, top, r.right - 8.0, top + 44.0);
                if i == self.bar.sel {
                    dc.FillRoundedRectangle(&rr(row, 12.0), &sel);
                } else if self.bar.hover == Some(i) {
                    dc.FillRoundedRectangle(&rr(row, 12.0), &hov);
                }
                let (main, detail, url) = describe(item);
                let (ix, iy) = (row.left + 12.0, top + 14.0);
                match url.and_then(|u| icons.get(&crate::app::host_of(u))) {
                    Some((color, _)) => {
                        if let Ok(bmp) = dc.CreateBitmapFromWicBitmap(color, None) {
                            dc.DrawBitmap(&bmp, Some(&rect(ix, iy, ix + 16.0, iy + 16.0)), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
                        }
                    }
                    None => {
                        let ic = if matches!(item, Suggestion::Go { is_search: true, .. }) { Icon::Search } else { Icon::Globe };
                        draw_icon(&self.gfx, rt, ic, ix, iy, &fg2);
                    }
                }
                let main_w: Vec<u16> = main.encode_utf16().collect();
                let detail_w: Vec<u16> = detail.encode_utf16().collect();
                let chip_w = if i == self.bar.sel { chip_width(enter_label) + 12.0 } else { 0.0 };
                let tl = row.left + 40.0;
                let tr = row.right - 12.0 - chip_w;
                let split = tl + (tr - tl) * 0.6;
                let main_r = if detail.is_empty() { rect(tl, top, tr, top + 44.0) } else { rect(tl, top, split - 10.0, top + 44.0) };
                dc.DrawText(&main_w, &self.fonts.title, &main_r, &fg, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                if !detail.is_empty() {
                    dc.DrawText(&detail_w, &self.fonts.small, &rect(split, top, tr, top + 44.0), &fg3, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
                }
                if i == self.bar.sel {
                    self.draw_chips(dc, &[enter_label.to_string()], row.right - 12.0 - chip_width(enter_label), top + 12.0, t);
                }
            }
            dc.SetTransform(&old);
        }
    }

    fn draw_tip(&self, dc: &ID2D1DeviceContext, tip: &Tip, a: f32) {
        let th = self.theme;
        let label: Vec<u16> = tip.label.encode_utf16().collect();
        let label_w = self.text_width(&label, &self.fonts.small);
        let chips_w: f32 = tip.keys.iter().map(|k| chip_width(k) + 4.0).sum();
        let w = 12.0 + label_w + if tip.keys.is_empty() { 0.0 } else { 10.0 + chips_w } + 8.0;
        let h = 30.0;
        let r = rect(tip.x, tip.y, tip.x + w, tip.y + h);
        let b = |c: u32, al: f32| unsafe { dc.CreateSolidColorBrush(&rgba(c, al * a), None).ok() };
        unsafe {
            if let Some(sh) = b(0x000000, 0.12) {
                dc.FillRoundedRectangle(&rr(rect(r.left, r.top + 2.0, r.right, r.bottom + 3.0), 10.0), &sh);
            }
            if let (Some(bg), Some(line), Some(fg)) = (b(th.panel(), 0.98), b(th.ink(), 0.10), b(th.fg(), 1.0)) {
                dc.FillRoundedRectangle(&rr(r, 10.0), &bg);
                dc.DrawRoundedRectangle(&rr(r, 10.0), &line, 1.0, None);
                dc.DrawText(&label, &self.fonts.small, &rect(r.left + 12.0, r.top, r.left + 12.0 + label_w + 2.0, r.bottom), &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
        }
        let mut x = r.left + 12.0 + label_w + 10.0;
        for k in &tip.keys {
            self.draw_chips(dc, std::slice::from_ref(k), x, r.top + 5.0, a);
            x += chip_width(k) + 4.0;
        }
    }

    /// Keyboard key chips ("Ctrl", "T"), as in Raycast.
    fn draw_chips(&self, dc: &ID2D1DeviceContext, keys: &[String], x: f32, y: f32, a: f32) {
        let th = self.theme;
        let mut x = x;
        for k in keys {
            let w = chip_width(k);
            let r = rect(x, y, x + w, y + 20.0);
            unsafe {
                if let (Ok(bg), Ok(fg)) = (
                    dc.CreateSolidColorBrush(&rgba(th.ink(), 0.09 * a), None),
                    dc.CreateSolidColorBrush(&rgba(th.fg(), 0.75 * a), None),
                ) {
                    dc.FillRoundedRectangle(&rr(r, 6.0), &bg);
                    let text: Vec<u16> = k.encode_utf16().collect();
                    dc.DrawText(&text, &self.fonts.chip, &r, &fg, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
            x += w + 4.0;
        }
    }

    fn text_width(&self, text: &[u16], font: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(l) = self.gfx.dwrite.CreateTextLayout(text, font, 10_000.0, 40.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }
}

fn chip_width(k: &str) -> f32 {
    (k.chars().count() as f32 * 7.0 + 12.0).max(20.0)
}

fn describe(item: &Suggestion) -> (String, String, Option<&String>) {
    match item {
        Suggestion::Go { input, is_search: true } => (format!("{} \u{201C}{input}\u{201D}", t("Cerca", "Search")), String::new(), None),
        Suggestion::Go { input, is_search: false } => (input.clone(), t("Vai all'indirizzo", "Go to address").into(), None),
        Suggestion::Tab { title, url, .. } => (title.clone(), t("Passa alla scheda", "Switch to tab").into(), Some(url)),
        Suggestion::Bookmark { title, url } | Suggestion::History { title, url } => {
            let t = if title.is_empty() { url.clone() } else { title.clone() };
            (t, short_url(url), Some(url))
        }
    }
}

pub fn short_url(url: &str) -> String {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.strip_prefix("www.").unwrap_or(u).trim_end_matches('/').to_string()
}

fn rr(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

fn circle(r: D2D_RECT_F) -> D2D1_ELLIPSE {
    let rad = (r.right - r.left).min(r.bottom - r.top) / 2.0;
    D2D1_ELLIPSE {
        point: windows_numerics::Vector2 { X: (r.left + r.right) / 2.0, Y: (r.top + r.bottom) / 2.0 },
        radiusX: rad,
        radiusY: rad,
    }
}

/// "Ctrl+Shift+T" → ["Ctrl", "Shift", "T"] with arrows as glyphs.
pub fn key_chips(combo: &str) -> Vec<String> {
    combo
        .split('+')
        .map(|k| match k.trim() {
            "Left" => "\u{2190}".into(),
            "Right" => "\u{2192}".into(),
            "Up" => "\u{2191}".into(),
            "Down" => "\u{2193}".into(),
            "Comma" => ",".into(),
            "Plus" => "+".into(),
            "Minus" => "-".into(),
            "Slash" => "/".into(),
            "PageUp" => "PgSu".into(),
            "PageDown" => "PgGi\u{f9}".into(),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_split_combos() {
        assert_eq!(key_chips("Ctrl+Shift+T"), vec!["Ctrl", "Shift", "T"]);
        assert_eq!(key_chips("Alt+Left"), vec!["Alt", "\u{2190}"]);
    }

    #[test]
    fn short_urls() {
        assert_eq!(short_url("https://www.youtube.com/"), "youtube.com");
        assert_eq!(short_url("https://a.b/c"), "a.b/c");
    }
}

/// Tab title, or its domain while it has none yet, or "New tab".
fn tab_label(t: &TabView) -> String {
    if !t.title.is_empty() {
        t.title.to_string()
    } else if !t.url.is_empty() && t.url != "about:blank" {
        crate::app::host_of(t.url).trim_start_matches("www.").to_string()
    } else {
        tr("Nuova scheda", "New tab").to_string()
    }
}
/// Width of a text pill button (DIPs) for `label`.
pub fn text_pill_width(label: &str) -> f32 {
    label.chars().count() as f32 * 6.6 + 28.0
}
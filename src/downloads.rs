//! Downloads with our own UI instead of Edge's: a card over the page while a
//! file downloads, a quick button next to the settings gear for two minutes
//! after the last download, and the full list in Settings.
//!
//! No SmartScreen ("not commonly downloaded", "report as safe"...): the
//! engine's reputation check is off. A download from a site on the scam /
//! malware block lists gets a plain warning with Keep or Delete.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{Foundation::HWND, Graphics::Direct2D::Common::D2D_RECT_F},
    core::*,
};

use crate::{
    app::{App, Event, TabId, post},
    gfx::{contains, rect},
    i18n::t,
};

/// The quick button stays this long after the last download.
const BUTTON_FOR: Duration = Duration::from_secs(120);
/// The card shows this long when a download starts, then flies into the
/// downloads button (the download goes on there)...
const CARD_MS: u32 = 2500;
/// ...or this long once a download ends.
const CARD_DONE_MS: u32 = 1400;
pub const TIMER_CARD: usize = 9;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum State {
    Running,
    Done,
    Failed,
    Deleted,
}

/// One download, as listed and saved (`downloads.json`, newest first).
#[derive(Serialize, Deserialize, Clone)]
pub struct Record {
    pub name: String,
    pub path: String,
    pub url: String,
    pub state: State,
    pub bytes: i64,
    pub total: i64,
    /// From a site on the scam/malware lists: kept only if the user says so.
    #[serde(default)]
    pub flagged: bool,
    #[serde(skip)]
    pub op: Option<ICoreWebView2DownloadOperation>,
    /// Open the file as soon as it is complete (clicked while downloading).
    #[serde(skip)]
    pub open_when_done: bool,
    #[serde(skip)]
    pub paused: bool,
}

/// What the card shows now.
pub struct Card {
    pub index: usize,
    pub hover: Option<CardButton>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CardButton {
    Open,
    Folder,
    Keep,
    Delete,
    Cancel,
    Close,
}

pub fn folder() -> PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None)
            .ok()
            .map(|p| PathBuf::from(CoTaskMemPWSTR::from(p).to_string()))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Opens a file or folder with its Windows app.
fn shell_open(target: &str, args: Option<&str>) {
    use windows::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let (file, params) = match args {
        Some(a) => (HSTRING::from(target), HSTRING::from(a)),
        None => (HSTRING::from(target), HSTRING::new()),
    };
    unsafe {
        ShellExecuteW(None, w!("open"), &file, &params, None, SW_SHOWNORMAL);
    }
}

pub fn size_text(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

impl Record {
    /// Percentage while downloading (what matters), size when done.
    pub fn status(&self) -> String {
        let pct = |d: &Record| (d.bytes as f64 * 100.0 / d.total.max(1) as f64).floor() as i64;
        match self.state {
            State::Running if self.paused => format!("{} {}%", t("In pausa", "Paused"), pct(self)),
            State::Running if self.total > 0 => format!("{}%", pct(self)),
            State::Running => size_text(self.bytes),
            State::Done => size_text(self.bytes.max(self.total)),
            State::Failed => t("Interrotto", "Interrupted").into(),
            State::Deleted => t("Eliminato", "Deleted").into(),
        }
    }

    pub fn progress(&self) -> Option<f32> {
        (self.state == State::Running && self.total > 0).then(|| (self.bytes as f32 / self.total as f32).clamp(0.0, 1.0))
    }
}

impl App {
    pub fn load_downloads(&mut self) {
        let path = self.data_dir.join("downloads.json");
        self.downloads = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // Downloads still running when the app closed did not finish (the
        // engine drops unfinished files): they are failed, not "running".
        // A file that is complete on disk counts as done.
        for d in &mut self.downloads {
            if d.state == State::Running {
                let complete = std::fs::metadata(&d.path).is_ok_and(|m| d.total > 0 && m.len() as i64 == d.total);
                d.state = if complete { State::Done } else { State::Failed };
            }
        }
    }

    fn save_downloads(&self) {
        let list: Vec<&Record> = self.downloads.iter().take(100).collect();
        if let Ok(json) = serde_json::to_string_pretty(&list) {
            let _ = std::fs::write(self.data_dir.join("downloads.json"), json);
        }
    }

    /// A page started a download (default UI suppressed by the handler).
    pub fn download_started(&mut self, tab: TabId, op: ICoreWebView2DownloadOperation, path: String) {
        let mut p = PWSTR::null();
        let url = unsafe { op.Uri(&mut p) }.map(|_| CoTaskMemPWSTR::from(p).to_string()).unwrap_or_default();
        let mut total = 0i64;
        unsafe {
            let _ = op.TotalBytesToReceive(&mut total);
        }
        let source = self.tabs.iter().find(|t| t.id == tab).map(|t| t.url.clone()).unwrap_or_default();
        let flagged = self.blocker.as_ref().is_some_and(|b| b.navigation_blocked(&url, &source) || b.popup_blocked(&url, &source));
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        if let Some(c) = &mut self.card {
            c.index += 1; // the list grows at the front
        }
        self.downloads.insert(0, Record { name, path, url, state: State::Running, bytes: 0, total, flagged, op: Some(op.clone()), open_when_done: false, paused: false });
        self.show_card(0, CARD_MS);
        self.last_download = Some(Instant::now());
        self.ui.downloads_button = true;
        watch_operation(self.hwnd, &op);
        self.save_downloads();
        self.redraw();
    }

    /// Bytes or state of a running download changed.
    pub fn download_progress(&mut self, op: ICoreWebView2DownloadOperation) {
        let Some(i) = self.downloads.iter().position(|d| d.op.as_ref() == Some(&op)) else { return };
        let (mut bytes, mut total, mut state) = (0i64, 0i64, COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS);
        unsafe {
            let _ = op.BytesReceived(&mut bytes);
            let _ = op.TotalBytesToReceive(&mut total);
            let _ = op.State(&mut state);
        }
        let mut can_resume = windows::core::BOOL(0);
        unsafe {
            let _ = op.CanResume(&mut can_resume);
        }
        let d = &mut self.downloads[i];
        d.bytes = bytes;
        d.total = total;
        // Interrupted but resumable (network drop, or paused): it can go on.
        if state == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED && can_resume.as_bool() {
            d.paused = true;
            self.refresh_downloads_menu();
            return self.redraw();
        }
        let ended = state != COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS;
        if ended {
            d.state = if state == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED { State::Done } else { State::Failed };
            if d.state == State::Failed && d.flagged {
                d.state = State::Deleted;
            }
            d.op = None;
            self.last_download = Some(Instant::now());
            // A flagged file comes back for Keep/Delete; others leave soon.
            if self.downloads[i].flagged && self.downloads[i].state == State::Done {
                self.show_card(i, 0);
            } else if self.card.as_ref().is_some_and(|c| c.index == i) {
                self.arm_card(CARD_DONE_MS);
            }
            if self.downloads[i].open_when_done && self.downloads[i].state == State::Done && !self.downloads[i].flagged {
                self.open_download(i);
            }
            self.save_downloads();
        }
        self.refresh_downloads_menu();
        self.redraw();
    }

    /// Shows the card for download `i`; it leaves by itself after `ms`
    /// (0: stays until answered, for flagged files).
    fn show_card(&mut self, i: usize, ms: u32) {
        self.card = Some(Card { index: i, hover: None });
        self.ui.card_out.set(0.0);
        if ms > 0 {
            self.arm_card(ms);
        }
        self.redraw();
    }

    fn arm_card(&mut self, ms: u32) {
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(self.hwnd), TIMER_CARD, ms, None);
        }
    }

    /// The card flies into the downloads button and disappears.
    pub fn card_leave(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(self.hwnd), TIMER_CARD);
        }
        let waiting = self.card.as_ref().and_then(|c| self.downloads.get(c.index)).is_some_and(|d| d.flagged && d.state == State::Done);
        if self.card.is_some() && !waiting {
            self.ui.downloads_button = true;
            self.last_download = Some(Instant::now());
            self.ui.card_out.glide(1.0, if self.ui.anims { 420 } else { 0 });
            self.redraw();
        }
    }

    /// End of the flight: the card is gone.
    pub fn card_landed(&mut self) {
        if self.card.is_some() && self.ui.card_out.target() >= 1.0 && !self.ui.card_out.running() {
            self.card = None;
            self.ui.card_out.set(0.0);
        }
    }

    /// Called by the 5 s tick: hide the quick button when its time is up.
    pub fn download_tick(&mut self) {
        let now = Instant::now();
        let show = self.downloads.iter().any(|d| d.state == State::Running) || self.last_download.is_some_and(|t| now - t < BUTTON_FOR);
        if show != self.ui.downloads_button {
            self.ui.downloads_button = show;
            self.redraw();
        }
    }

    // ---------- card ----------

    /// The card where the pointer can use it (not while it flies away).
    pub fn card_rect(&self) -> Option<D2D_RECT_F> {
        if self.ui.card_out.target() > 0.0 {
            return None;
        }
        self.card_box()
    }

    /// The card's resting place (drawn moving from there when it leaves).
    pub fn card_box(&self) -> Option<D2D_RECT_F> {
        self.card.as_ref()?;
        let p = self.page_rect();
        let s = self.scale;
        let (l, b) = (p.left as f32 / s + 16.0, p.bottom as f32 / s - 16.0);
        Some(rect(l, b - 104.0, l + 360.0, b))
    }

    /// Buttons for the card's download, right to left.
    pub fn card_buttons(&self, card: D2D_RECT_F) -> Vec<(D2D_RECT_F, CardButton, &'static str)> {
        let Some(d) = self.card.as_ref().and_then(|c| self.downloads.get(c.index)) else { return Vec::new() };
        let mut labels: Vec<(CardButton, &'static str)> = match (d.state, d.flagged) {
            (State::Running, _) => vec![(CardButton::Cancel, t("Annulla", "Cancel"))],
            (State::Done, true) => vec![(CardButton::Keep, t("Mantieni", "Keep")), (CardButton::Delete, t("Elimina", "Delete"))],
            (State::Done, false) => vec![(CardButton::Open, t("Apri", "Open")), (CardButton::Folder, t("Mostra nella cartella", "Show in folder"))],
            _ => Vec::new(),
        };
        let mut out = Vec::new();
        let mut right = card.right - 14.0;
        let y = card.bottom - 44.0;
        for (b, label) in labels.drain(..) {
            let w = crate::ui::text_pill_width(label) + 6.0;
            out.push((rect(right - w, y, right, y + 30.0), b, label));
            right -= w + 8.0;
        }
        out.push((rect(card.right - 38.0, card.top + 10.0, card.right - 12.0, card.top + 36.0), CardButton::Close, ""));
        out
    }

    pub fn download_mouse(&mut self, kind: crate::shell::Mouse, x: i32, y: i32) -> bool {
        use crate::shell::Mouse;
        let Some(card) = self.card_rect() else { return false };
        let (xd, yd) = (x as f32 / self.scale, y as f32 / self.scale);
        if !contains(&card, xd, yd) {
            if let Some(c) = &mut self.card
                && c.hover.is_some()
            {
                c.hover = None;
                self.redraw();
            }
            return false;
        }
        let hover = self.card_buttons(card).into_iter().find(|(r, ..)| contains(r, xd, yd)).map(|(_, b, _)| b);
        if let Some(c) = &mut self.card
            && c.hover != hover
        {
            c.hover = hover;
            self.redraw();
        }
        if kind == Mouse::LeftUp
            && let Some(b) = hover
        {
            self.card_action(b);
        }
        true
    }

    fn card_action(&mut self, b: CardButton) {
        let Some(i) = self.card.as_ref().map(|c| c.index) else { return };
        match b {
            CardButton::Close => self.card_leave(),
            CardButton::Open => {
                self.open_download(i);
                self.card_leave();
            }
            CardButton::Folder => {
                self.show_download(i);
                self.card_leave();
            }
            CardButton::Cancel => {
                if let Some(op) = self.downloads.get(i).and_then(|d| d.op.clone()) {
                    unsafe {
                        let _ = op.Cancel();
                    }
                }
            }
            CardButton::Keep => {
                if let Some(d) = self.downloads.get_mut(i) {
                    d.flagged = false;
                }
                self.save_downloads();
                self.arm_card(CARD_DONE_MS);
            }
            CardButton::Delete => {
                self.delete_download(i);
                self.card = None;
            }
        }
        self.redraw();
    }

    // ---------- actions (card, quick menu, settings) ----------

    /// "Download image": straight into the downloads folder (no Save As
    /// dialog), listed and shown like any download.
    pub fn download_direct(&mut self, url: String, referer: String) {
        if !url.starts_with("http") {
            return;
        }
        let raw = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
        let raw = crate::settings::url_decode(&raw);
        let mut name: String = raw.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
        if name.trim().is_empty() {
            name = "immagine".into();
        }
        if !name.contains('.') {
            name.push_str(".jpg");
        }
        let dir = folder();
        let (stem, ext) = match name.rfind('.') {
            Some(i) => (name[..i].to_string(), name[i..].to_string()),
            None => (name.clone(), String::new()),
        };
        let mut path = dir.join(&name);
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        let path_s = path.to_string_lossy().into_owned();
        let shown = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(name);
        if let Some(c) = &mut self.card {
            c.index += 1;
        }
        self.downloads.insert(0, Record { name: shown, path: path_s.clone(), url: url.clone(), state: State::Running, bytes: 0, total: 0, flagged: false, op: None, open_when_done: false, paused: false });
        self.last_download = Some(Instant::now());
        self.ui.downloads_button = true;
        self.show_card(0, CARD_MS);
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let ok = crate::sys::quiet_command("curl.exe")
                .args(["-sSfL", "--ssl-no-revoke", "--max-time", "120", "-e"])
                .arg(&referer)
                .arg("-o")
                .arg(&path_s)
                .arg(&url)
                .status()
                .is_ok_and(|s| s.success());
            let size = std::fs::metadata(&path_s).map(|m| m.len() as i64).unwrap_or(0);
            if !ok {
                let _ = std::fs::remove_file(&path_s);
            }
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(HWND(hwnd as _)),
                    windows::Win32::UI::WindowsAndMessaging::WM_APP + 5,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(Box::into_raw(Box::new((path_s, ok, size))) as isize),
                );
            }
        });
    }

    pub fn direct_done(&mut self, path: &str, ok: bool, size: i64) {
        let Some(i) = self.downloads.iter().position(|d| d.path == path && d.state == State::Running) else { return };
        let d = &mut self.downloads[i];
        d.state = if ok { State::Done } else { State::Failed };
        d.bytes = size;
        d.total = size;
        self.last_download = Some(Instant::now());
        if self.card.as_ref().is_some_and(|c| c.index == i) {
            self.arm_card(CARD_DONE_MS);
        }
        self.save_downloads();
        self.refresh_downloads_menu();
        self.redraw();
    }

    /// Opens a finished file (any type: .exe runs its installer). A file
    /// still downloading is opened as soon as it is complete.
    pub fn open_download(&mut self, i: usize) {
        let Some(d) = self.downloads.get_mut(i) else { return };
        match d.state {
            State::Done => shell_open(&d.path, None),
            State::Running => d.open_when_done = true,
            _ => {}
        }
    }

    pub fn show_download(&mut self, i: usize) {
        match self.downloads.get(i).filter(|d| std::path::Path::new(&d.path).exists()) {
            Some(d) => shell_open("explorer.exe", Some(&format!("/select,\"{}\"", d.path))),
            None => self.open_downloads_folder(),
        }
    }

    pub fn open_downloads_folder(&mut self) {
        shell_open(&folder().to_string_lossy(), None);
    }

    pub fn delete_download(&mut self, i: usize) {
        let Some(d) = self.downloads.get_mut(i) else { return };
        if let Some(op) = d.op.take() {
            unsafe {
                let _ = op.Cancel();
            }
        }
        let _ = std::fs::remove_file(&d.path);
        d.state = State::Deleted;
        d.flagged = false;
        self.save_downloads();
    }

    /// The quick button: recent downloads as a menu.
    /// The quick button's menu: recent downloads with a live percentage.
    /// A finished file opens on click; a running one opens its own menu.
    pub fn open_downloads_menu(&mut self, x: f32, y: f32) {
        let items = self.downloads_menu_items();
        self.show_menu(items, x, y);
        self.downloads_menu = true;
    }

    fn downloads_menu_items(&self) -> Vec<crate::ui::MenuItem> {
        use crate::ui::{MenuCmd, MenuItem};
        let mut items: Vec<MenuItem> = self
            .downloads
            .iter()
            .enumerate()
            .take(8)
            .map(|(i, d)| {
                let cmd = if d.state == State::Running { MenuCmd::DownloadMenu(i) } else { MenuCmd::OpenDownload(i) };
                let keys = if d.open_when_done && d.state == State::Running { format!("{} \u{00B7} {}", d.status(), t("si apre", "opens")) } else { d.status() };
                MenuItem { label: d.name.clone(), cmd, keys, sep: false }
            })
            .collect();
        items.push(MenuItem { label: t("Apri la cartella dei download", "Open downloads folder").into(), cmd: MenuCmd::DownloadsFolder, keys: String::new(), sep: !items.is_empty() });
        items.push(MenuItem { label: t("Tutti i download", "All downloads").into(), cmd: MenuCmd::AllDownloads, keys: String::new(), sep: false });
        items
    }

    /// Live percentages: the open downloads menu follows the progress.
    fn refresh_downloads_menu(&mut self) {
        if !self.downloads_menu {
            return;
        }
        let items = self.downloads_menu_items();
        match &mut self.ui.menu {
            Some(m) if m.items.len() == items.len() => m.items = items,
            _ => self.downloads_menu = false,
        }
    }

    /// A running download: open when done, pause / resume, cancel.
    pub fn open_download_menu(&mut self, i: usize) {
        use crate::ui::{MenuCmd, MenuItem};
        let Some(d) = self.downloads.get(i) else { return };
        let (x, y) = self.ui.menu_anchor;
        let check = if d.open_when_done { "\u{2713}" } else { "" };
        let items = vec![
            MenuItem { label: d.name.clone(), cmd: MenuCmd::DownloadMenu(i), keys: d.status(), sep: false },
            MenuItem { label: t("Apri al termine", "Open when done").into(), cmd: MenuCmd::DownloadOpenWhenDone(i), keys: check.into(), sep: true },
            MenuItem {
                label: if d.paused { t("Riprendi", "Resume") } else { t("Pausa", "Pause") }.into(),
                cmd: MenuCmd::DownloadPause(i),
                keys: String::new(),
                sep: false,
            },
            MenuItem { label: t("Annulla il download", "Cancel download").into(), cmd: MenuCmd::DownloadCancel(i), keys: String::new(), sep: false },
        ];
        self.show_menu(items, x, y);
    }

    pub fn toggle_open_when_done(&mut self, i: usize) {
        if let Some(d) = self.downloads.get_mut(i) {
            d.open_when_done = !d.open_when_done;
        }
    }

    pub fn pause_download(&mut self, i: usize) {
        let Some(d) = self.downloads.get_mut(i) else { return };
        let Some(op) = d.op.clone() else { return };
        unsafe {
            if d.paused {
                let _ = op.Resume();
            } else {
                let _ = op.Pause();
            }
        }
        d.paused = !d.paused;
        self.redraw();
    }

    pub fn cancel_download(&mut self, i: usize) {
        if let Some(op) = self.downloads.get(i).and_then(|d| d.op.clone()) {
            unsafe {
                let _ = op.Cancel();
            }
        }
    }

    /// Settings page: list / open / show / folder / clear.
    pub fn downloads_message(&mut self, tab: TabId, msg: &serde_json::Value) {
        let i = msg.get("i").and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
        match msg.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "dl_open" => self.open_download(i),
            "dl_show" => self.show_download(i),
            "dl_folder" => self.open_downloads_folder(),
            "dl_clear" => {
                self.downloads.retain(|d| d.state == State::Running);
                self.save_downloads();
            }
            _ => {}
        }
        let list: Vec<_> = self
            .downloads
            .iter()
            .map(|d| serde_json::json!({ "name": d.name, "status": d.status(), "done": d.state == State::Done, "url": d.url }))
            .collect();
        let reply = serde_json::json!({ "type": "downloads", "list": list, "folder": folder().to_string_lossy() });
        if let Some(v) = self.tabs.iter().find(|t| t.id == tab).and_then(|t| t.view.as_ref()) {
            unsafe {
                let _ = v.webview.PostWebMessageAsJson(&HSTRING::from(reply.to_string()));
            }
        }
    }
}

/// Wires a page's DownloadStarting: no Edge download bubble.
pub fn watch(webview: &ICoreWebView2, id: TabId, hwnd: crate::app::Owner) -> windows::core::Result<()> {
    let Ok(wv4) = webview.cast::<ICoreWebView2_4>() else { return Ok(()) };
    let mut token = 0;
    unsafe {
        // Executables (.exe, .msi...) otherwise wait for Edge's "keep or
        // discard?" prompt, which never shows with our UI: the download
        // stalls at 100% and the file is thrown away when the app closes.
        // Our block lists flag risky sources instead (see `flagged`).
        if let Ok(wv26) = webview.cast::<ICoreWebView2_26>() {
            wv26.add_SaveFileSecurityCheckStarting(
                &SaveFileSecurityCheckStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetSuppressDefaultPolicy(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        wv4.add_DownloadStarting(
            &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                args.SetHandled(true)?;
                let op = args.DownloadOperation()?;
                let mut p = PWSTR::null();
                args.ResultFilePath(&mut p)?;
                post(hwnd, Event::DownloadStarted(id, op, CoTaskMemPWSTR::from(p).to_string()));
                Ok(())
            })),
            &mut token,
        )
    }
}

fn watch_operation(hwnd: HWND, op: &ICoreWebView2DownloadOperation) {
    let mut token = 0;
    unsafe {
        let _ = op.add_BytesReceivedChanged(
            &BytesReceivedChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(op) = sender {
                    post(hwnd, Event::DownloadProgress(op));
                }
                Ok(())
            })),
            &mut token,
        );
        let _ = op.add_StateChanged(
            &StateChangedEventHandler::create(Box::new(move |sender, _| {
                if let Some(op) = sender {
                    post(hwnd, Event::DownloadProgress(op));
                }
                Ok(())
            })),
            &mut token,
        );
    }
}

//! Browser extensions from the Chrome Web Store and Edge Add-ons.
//!
//! WebView2 cannot install from the stores itself (their "Add" button
//! needs Chrome's private store API, so the download is "interrupted").
//! Instead the store page's button is intercepted: the extension package
//! is downloaded, unpacked into the data folder and loaded with
//! `ICoreWebView2Profile7::AddBrowserExtension`. The settings page lists
//! them (enable, remove, open). Chromium no longer runs Manifest V2
//! extensions, so those are refused with a clear message.

use std::path::{Path, PathBuf};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
    core::*,
};

use crate::{
    app::{App, TabId},
    i18n::t,
};

/// Posted by the download thread; LPARAM is a `Box<Unpacked>`.
pub const WM_EXT_UNPACKED: u32 = WM_APP + 3;
/// "Load from folder": the folder dialog is modal, so it runs from the
/// window procedure, outside any app borrow.
pub const WM_EXT_PICK: u32 = WM_APP + 4;

/// Folder picker (for unpacked extensions).
pub fn pick_folder(hwnd: HWND) -> Option<PathBuf> {
    use windows::Win32::{
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH},
    };
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dlg.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dlg.SetTitle(&HSTRING::from(t("Cartella dell'estensione (con manifest.json)", "Extension folder (with manifest.json)")));
        dlg.Show(Some(hwnd)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        Some(PathBuf::from(CoTaskMemPWSTR::from(p).to_string()))
    }
}

/// An installed (user) extension, as the extensions button lists it.
pub struct Ext {
    pub id: String,
    pub name: String,
    /// What the engine currently runs (the policy for the active site).
    pub enabled: bool,
    pub handle: ICoreWebView2BrowserExtension,
}

/// On/off per extension: a general choice (settings page) and per-site
/// overrides (extensions button). WebView2 can only switch an extension on
/// or off everywhere, so the active page's choice is applied whenever the
/// active page changes. Stored in `extensions/sites.json`.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub general: std::collections::HashMap<String, bool>,
    #[serde(default)]
    pub sites: std::collections::HashMap<String, std::collections::HashMap<String, bool>>,
}

impl Policy {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(data_dir.join("extensions").join("sites.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) {
        let dir = data_dir.join("extensions");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("sites.json"), serde_json::to_string_pretty(self).unwrap_or_default());
    }

    pub fn general(&self, id: &str) -> bool {
        self.general.get(id).copied().unwrap_or(true)
    }

    pub fn site(&self, id: &str, host: &str) -> Option<bool> {
        self.sites.get(id).and_then(|m| m.get(host)).copied()
    }

    pub fn wanted(&self, id: &str, host: &str) -> bool {
        self.site(id, host).unwrap_or_else(|| self.general(id))
    }
}

pub struct Unpacked {
    pub id: String,
    pub result: std::result::Result<PathBuf, String>,
}

/// Injected in every page; acts only on the two extension stores. A click
/// on the store's install button asks the browser to install instead.
pub fn store_script(token: &str) -> String {
    format!(
        r#"(() => {{
  if (window.top !== window || !(window.chrome && chrome.webview)) return;
  const h = location.hostname;
  const cws = h === 'chromewebstore.google.com' || h === 'chrome.google.com';
  const edge = h === 'microsoftedge.microsoft.com';
  if (!cws && !edge) return;
  const idOf = () => (location.pathname.match(/\/([a-p]{{32}})(?:[/?#]|$)/) || [])[1];
  const words = /^(aggiungi( a chrome)?|add( to chrome)?|ottieni|get|installa|install)$/i;
  document.addEventListener('click', (e) => {{
    const b = e.target && e.target.closest && e.target.closest('button,[role=button],a');
    const id = idOf();
    if (!b || !id || !words.test((b.innerText || b.textContent || '').trim())) return;
    e.preventDefault(); e.stopImmediatePropagation();
    chrome.webview.postMessage({{ k: {token}, install: id, store: edge ? 'edge' : 'chrome' }});
  }}, true);
}})();"#,
        token = serde_json::to_string(token).unwrap_or_default()
    )
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

fn crx_url(id: &str, edge: bool) -> String {
    if edge {
        format!("https://edge.microsoft.com/extensionwebstorebase/v1/crx?response=redirect&x=id%3D{id}%26installsource%3Dondemand%26uc")
    } else {
        // prodversion: a current Chrome major, or the store refuses.
        format!("https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0&acceptformat=crx2,crx3&x=id%3D{id}%26uc")
    }
}

/// The ZIP inside a CRX package (versions 2 and 3).
fn crx_zip(crx: &[u8]) -> Option<&[u8]> {
    let u32_at = |i: usize| crx.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if crx.get(..4)? != b"Cr24" {
        return None;
    }
    let start = match u32_at(4)? {
        3 => 12 + u32_at(8)?,
        2 => 16 + u32_at(8)? + u32_at(12)?,
        _ => return None,
    };
    crx.get(start..).filter(|z| z.starts_with(b"PK"))
}

/// Downloads and unpacks into `<data>/extensions/<id>` (worker thread).
fn fetch(dir: &Path, id: &str, edge: bool) -> std::result::Result<PathBuf, String> {
    let _ = std::fs::create_dir_all(dir);
    let crx = dir.join(format!("{id}.crx"));
    let ok = crate::sys::quiet_command("curl.exe")
        .args(["-sSfL", "--ssl-no-revoke", "--max-time", "60", "-o"])
        .arg(&crx)
        .arg(crx_url(id, edge))
        .status()
        .is_ok_and(|s| s.success());
    let bytes = std::fs::read(&crx).ok().filter(|_| ok).ok_or_else(|| t("Download non riuscito", "Download failed").to_string())?;
    let _ = std::fs::remove_file(&crx);
    let zip = crx_zip(&bytes).ok_or_else(|| t("Pacchetto non valido", "Invalid package").to_string())?;
    let out = dir.join(id);
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let zip_path = dir.join(format!("{id}.zip"));
    std::fs::write(&zip_path, zip).map_err(|e| e.to_string())?;
    // Windows' own tar (bsdtar) reads ZIP files.
    let ok = crate::sys::quiet_command("tar.exe").arg("-xf").arg(&zip_path).arg("-C").arg(&out).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(&zip_path);
    if !ok || !out.join("manifest.json").exists() {
        return Err(t("Pacchetto non valido", "Invalid package").into());
    }
    // Chromium dropped Manifest V2: say so instead of a cryptic failure.
    let manifest: serde_json::Value = std::fs::read_to_string(out.join("manifest.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if manifest.get("manifest_version").and_then(|v| v.as_u64()) == Some(2) {
        let _ = std::fs::remove_dir_all(&out);
        return Err(t("Estensione troppo vecchia (Manifest V2), non più supportata", "Extension too old (Manifest V2), no longer supported").into());
    }
    Ok(out)
}

impl App {
    fn extensions_dir(&self) -> PathBuf {
        self.data_dir.join("extensions")
    }

    /// The profile of any live page (all tabs share one profile).
    fn profile(&self) -> Option<ICoreWebView2Profile7> {
        self.tabs.iter().filter_map(|t| t.view.as_ref()).find_map(|v| unsafe {
            v.webview.cast::<ICoreWebView2_13>().ok()?.Profile().ok()?.cast::<ICoreWebView2Profile7>().ok()
        })
    }

    pub fn install_extension(&mut self, id: String, edge: bool) {
        if !valid_id(&id) {
            return;
        }
        self.notify(t("Installazione dell'estensione…", "Installing the extension…"));
        let dir = self.extensions_dir();
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let result = fetch(&dir, &id, edge);
            let msg = Box::into_raw(Box::new(Unpacked { id, result }));
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as _)), WM_EXT_UNPACKED, WPARAM(0), LPARAM(msg as isize));
            }
        });
    }

    /// On the UI thread, once the files are on disk.
    pub fn extension_unpacked(&mut self, u: Unpacked) {
        let path = match u.result {
            Ok(p) => p,
            Err(e) => return self.notify(&e),
        };
        let Some(profile) = self.profile() else {
            return self.notify(t("Apri una pagina e riprova", "Open a page and try again"));
        };
        let hwnd = self.hwnd.0 as isize;
        let (dir, folder) = (self.extensions_dir(), u.id);
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |err, ext| {
            let text = match (err, ext) {
                (Ok(()), Some(ext)) => {
                    // Unpacked extensions get an id from their path: remember
                    // which folder it is.
                    let mut p = PWSTR::null();
                    if unsafe { ext.Id(&mut p) }.is_ok() {
                        let mut index = read_index(&dir);
                        index.insert(CoTaskMemPWSTR::from(p).to_string(), folder.clone());
                        let _ = std::fs::write(dir.join("index.json"), serde_json::to_string(&index).unwrap_or_default());
                    }
                    let mut p = PWSTR::null();
                    let name = unsafe { ext.Name(&mut p) }.map(|_| CoTaskMemPWSTR::from(p).to_string()).unwrap_or_default();
                    crate::app::post(HWND(hwnd as _), crate::app::Event::ExtensionsChanged);
                    format!("{} {name}", t("Estensione installata:", "Extension installed:"))
                }
                _ => t("Questa estensione non funziona in questo browser", "This extension does not work in this browser").to_string(),
            };
            crate::app::post(HWND(hwnd as _), crate::app::Event::Notice(text));
            Ok(())
        }));
        unsafe {
            if profile.AddBrowserExtension(&HSTRING::from(path.as_os_str()), &handler).is_err() {
                self.notify(t("Questa estensione non funziona in questo browser", "This extension does not work in this browser"));
            }
        }
    }

    /// "Load from folder" result: the user's own folder is loaded in place.
    pub fn load_unpacked(&mut self, path: PathBuf) {
        if !path.join("manifest.json").exists() {
            return self.notify(t("Nella cartella manca manifest.json", "The folder has no manifest.json"));
        }
        let folder = path.to_string_lossy().into_owned();
        self.extension_unpacked(Unpacked { id: folder, result: Ok(path) });
    }

    /// Settings page: list / enable / remove / open extensions.
    pub fn extensions_message(&mut self, tab: TabId, msg: &serde_json::Value) {
        let action = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "ext_store" => {
                let id = self.add_tab("https://chromewebstore.google.com/category/extensions".into(), String::new(), false);
                return self.activate(id);
            }
            "ext_folder" => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_EXT_PICK, WPARAM(0), LPARAM(0));
                return;
            },
            _ => {}
        }
        let Some(profile) = self.profile() else { return };
        let target = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let enable = msg.get("on").and_then(|v| v.as_bool()).unwrap_or(true);
        if action == "ext_enable" {
            self.ext_policy.general.insert(target.clone(), enable);
            self.ext_policy.save(&self.data_dir);
            self.apply_extensions();
        }
        if action == "ext_remove" {
            self.ext_policy.general.remove(&target);
            self.ext_policy.sites.remove(&target);
            self.ext_policy.save(&self.data_dir);
        }
        let general = self.ext_policy.general.clone();
        let webview = self.tabs.iter().find(|t| t.id == tab).and_then(|t| t.view.as_ref()).map(|v| v.webview.clone());
        let dir = self.extensions_dir();
        let action = action.to_string();
        let hwnd = self.hwnd.0 as isize;
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |err, list| {
            err?;
            let Some(list) = list else { return Ok(()) };
            let mut count = 0u32;
            unsafe { list.Count(&mut count)? };
            let mut items = Vec::new();
            // Only what the user installed: WebView2's own built-ins
            // (PDF viewer, clipboard) are part of the engine.
            let mine = read_index(&dir);
            for i in 0..count {
                let ext = unsafe { list.GetValueAtIndex(i)? };
                let (mut p_id, mut p_name, mut on) = (PWSTR::null(), PWSTR::null(), BOOL(0));
                unsafe {
                    ext.Id(&mut p_id)?;
                    ext.Name(&mut p_name)?;
                    ext.IsEnabled(&mut on)?;
                }
                let (id, name) = (CoTaskMemPWSTR::from(p_id).to_string(), CoTaskMemPWSTR::from(p_name).to_string());
                if !mine.contains_key(&id) {
                    continue;
                }
                if id == target {
                    match action.as_str() {
                        "ext_enable" => on = BOOL::from(enable),
                        "ext_remove" => {
                            unsafe { ext.Remove(&BrowserExtensionRemoveCompletedHandler::create(Box::new(|_| Ok(()))))? };
                            let mut index = read_index(&dir);
                            if let Some(folder) = index.remove(&id) {
                                // Only our own copies; a folder the user loaded stays.
                                if !Path::new(&folder).is_absolute() {
                                    let _ = std::fs::remove_dir_all(dir.join(folder));
                                }
                                let _ = std::fs::write(dir.join("index.json"), serde_json::to_string(&index).unwrap_or_default());
                            }
                            crate::app::post(HWND(hwnd as _), crate::app::Event::ExtensionsChanged);
                            continue;
                        }
                        "ext_open" => {
                            if let Some(page) = popup_page(&dir, &id) {
                                crate::app::post(HWND(hwnd as _), crate::app::Event::OpenUrl(format!("chrome-extension://{id}/{page}")));
                            }
                        }
                        _ => {}
                    }
                }
                let openable = popup_page(&dir, &id).is_some();
                let general = if id == target && action == "ext_enable" { on.as_bool() } else { general.get(&id).copied().unwrap_or(true) };
                items.push(serde_json::json!({ "id": id, "name": name, "on": general, "open": openable }));
            }
            if let Some(wv) = &webview {
                let reply = serde_json::json!({ "type": "extensions", "list": items });
                unsafe { wv.PostWebMessageAsJson(&HSTRING::from(reply.to_string()))? };
            }
            Ok(())
        }));
        unsafe {
            let _ = profile.GetBrowserExtensions(&handler);
        }
    }

    /// Reloads the installed extensions list (the extensions button).
    pub fn refresh_extensions(&mut self) {
        if self.private {
            return; // no extensions in a private window
        }
        let Some(profile) = self.profile() else { return };
        let mine = read_index(&self.extensions_dir());
        let hwnd = self.hwnd.0 as isize;
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |err, list| {
            err?;
            let Some(list) = list else { return Ok(()) };
            let mut count = 0u32;
            unsafe { list.Count(&mut count)? };
            let mut out = Vec::new();
            for i in 0..count {
                let handle = unsafe { list.GetValueAtIndex(i)? };
                let (mut p_id, mut p_name, mut on) = (PWSTR::null(), PWSTR::null(), BOOL(0));
                unsafe {
                    handle.Id(&mut p_id)?;
                    handle.Name(&mut p_name)?;
                    handle.IsEnabled(&mut on)?;
                }
                let id = CoTaskMemPWSTR::from(p_id).to_string();
                if mine.contains_key(&id) {
                    out.push(Ext { id, name: CoTaskMemPWSTR::from(p_name).to_string(), enabled: on.as_bool(), handle });
                }
            }
            crate::app::post(HWND(hwnd as _), crate::app::Event::Extensions(out));
            Ok(())
        }));
        unsafe {
            let _ = profile.GetBrowserExtensions(&handler);
        }
    }

    pub fn extensions_loaded(&mut self, list: Vec<Ext>) {
        self.exts = list;
        self.exts_loaded = true;
        self.apply_extensions();
        self.redraw();
    }

    /// Switches each extension to what the active page's site wants.
    pub fn apply_extensions(&mut self) {
        if self.private {
            return;
        }
        let host = self.active_index().map(|i| crate::app::host_of(&self.tabs[i].url)).unwrap_or_default();
        if host.is_empty() {
            return;
        }
        for e in &mut self.exts {
            let want = self.ext_policy.wanted(&e.id, &host);
            if e.enabled != want {
                e.enabled = want;
                unsafe {
                    let _ = e.handle.Enable(want, &BrowserExtensionEnableCompletedHandler::create(Box::new(|_| Ok(()))));
                }
            }
        }
    }

    /// The extensions button: one row per extension with its state here.
    pub fn open_extensions_menu(&mut self, x: f32, y: f32) {
        use crate::ui::{MenuCmd, MenuItem};
        let host = self.active_host();
        let mut items: Vec<MenuItem> = self
            .exts
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let state = match self.ext_policy.site(&e.id, &host) {
                    Some(true) => t("Attiva qui", "On here"),
                    Some(false) => t("Spenta qui", "Off here"),
                    None if self.ext_policy.general(&e.id) => t("Attiva", "On"),
                    None => t("Spenta", "Off"),
                };
                MenuItem { label: e.name.clone(), cmd: MenuCmd::ExtPick(i), keys: state.into(), sep: false }
            })
            .collect();
        if items.is_empty() {
            items.push(MenuItem { label: t("Aggiungi estensioni dallo store", "Add extensions from the store").into(), cmd: MenuCmd::ExtStore, keys: String::new(), sep: false });
        }
        items.push(MenuItem { label: t("Gestisci estensioni", "Manage extensions").into(), cmd: MenuCmd::ManageExtensions, keys: String::new(), sep: true });
        self.show_menu(items, x, y);
    }

    /// Second level: this extension on this site.
    pub fn open_extension_site_menu(&mut self, i: usize, x: f32, y: f32) {
        use crate::ui::{MenuCmd, MenuItem};
        let Some(e) = self.exts.get(i) else { return };
        let host = self.active_host();
        let site = self.ext_policy.site(&e.id, &host);
        let general = if self.ext_policy.general(&e.id) { t("attiva", "on") } else { t("spenta", "off") };
        let check = |on: bool| if on { "\u{2713}".to_string() } else { String::new() };
        let short = host.strip_prefix("www.").unwrap_or(&host).to_string();
        let mut items = vec![
            // Which extension this is about (clicking it changes nothing).
            MenuItem { label: e.name.clone(), cmd: MenuCmd::ExtPick(i), keys: String::new(), sep: false },
            MenuItem { label: format!("{} ({general})", t("Come in generale", "As in general")), cmd: MenuCmd::ExtSite(i, None), keys: check(site.is_none()), sep: true },
            MenuItem { label: format!("{} {short}", t("Sempre attiva su", "Always on for")), cmd: MenuCmd::ExtSite(i, Some(true)), keys: check(site == Some(true)), sep: false },
            MenuItem { label: format!("{} {short}", t("Sempre spenta su", "Always off for")), cmd: MenuCmd::ExtSite(i, Some(false)), keys: check(site == Some(false)), sep: false },
        ];
        if popup_page(&self.extensions_dir(), &e.id).is_some() {
            items.push(MenuItem { label: t("Opzioni dell'estensione", "Extension options").into(), cmd: MenuCmd::ExtOpen(i), keys: String::new(), sep: true });
        }
        self.show_menu(items, x, y);
    }

    pub fn set_extension_site(&mut self, i: usize, on: Option<bool>) {
        let host = self.active_host();
        let Some(e) = self.exts.get(i) else { return };
        if host.is_empty() {
            return;
        }
        let sites = self.ext_policy.sites.entry(e.id.clone()).or_default();
        match on {
            Some(v) => {
                sites.insert(host, v);
            }
            None => {
                sites.remove(&host);
            }
        }
        self.ext_policy.save(&self.data_dir);
        self.apply_extensions();
        // Content scripts only change on the next load.
        if let Some(v) = self.active_index().and_then(|i| self.tabs[i].view.as_ref()) {
            unsafe {
                let _ = v.webview.Reload();
            }
        }
    }

    pub fn open_extension_options(&mut self, i: usize) {
        let Some(e) = self.exts.get(i) else { return };
        if let Some(page) = popup_page(&self.extensions_dir(), &e.id) {
            let id = self.add_tab(format!("chrome-extension://{}/{page}", e.id), String::new(), false);
            self.activate(id);
        }
    }

    fn active_host(&self) -> String {
        self.active_index().map(|i| crate::app::host_of(&self.tabs[i].url)).unwrap_or_default()
    }

    /// Short message over the page (bottom center), gone after a few seconds.
    pub fn notify(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(self.hwnd), crate::shell::TIMER_NOTICE, 4000, None);
        }
        self.redraw();
    }

    pub fn clear_notice(&mut self) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(self.hwnd), crate::shell::TIMER_NOTICE);
        }
        self.notice = None;
        self.redraw();
    }
}

/// The extension's options page (or its toolbar popup), opened in a tab
/// since WebView2 has no extension toolbar.
fn popup_page(dir: &Path, id: &str) -> Option<String> {
    let folder = read_index(dir).remove(id)?;
    let text = std::fs::read_to_string(dir.join(folder).join("manifest.json")).ok()?;
    let m: serde_json::Value = serde_json::from_str(&text).ok()?;
    let pick = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
    // Options first: a toolbar popup opened as a tab has no page to act on.
    pick(m.pointer("/options_ui/page"))
        .or_else(|| pick(m.get("options_page")))
        .or_else(|| pick(m.pointer("/action/default_popup")))
        .or_else(|| pick(m.pointer("/browser_action/default_popup")))
}

/// Runtime extension id -> folder (store id) under `extensions/`.
fn read_index(dir: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(dir.join("index.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crx3_zip_offset() {
        let mut crx = b"Cr24".to_vec();
        crx.extend(3u32.to_le_bytes());
        crx.extend(4u32.to_le_bytes());
        crx.extend([0u8; 4]);
        crx.extend(b"PK\x03\x04rest");
        assert_eq!(crx_zip(&crx).map(|z| &z[..2]), Some(&b"PK"[..]));
        assert!(crx_zip(b"nope").is_none());
        assert!(valid_id("cjpalhdlnbpafiamejdnhcphjbkeiagm"));
        assert!(!valid_id("../../x"));
    }
}

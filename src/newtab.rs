//! The new tab page's own background (an image or a video the user picked):
//! copied into `<data>/background/` under a random name and served by
//! WebView2 at `https://veyra.background/<file>` (a folder mapping, which
//! also answers the byte ranges videos need). The page reads it with CORS
//! to measure its brightness; the random name keeps other sites from
//! guessing it.

use std::{
    hash::{BuildHasher, Hasher},
    path::{Path, PathBuf},
};

use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    Win32::{Foundation::HWND, UI::WindowsAndMessaging::WM_APP},
    core::*,
};

use crate::app::App;

/// Posted by the pages' "pick a file" button; the dialog opens in the
/// window procedure, outside any app borrow (see main.rs).
pub const WM_BG_PICK: u32 = WM_APP + 11;

pub const HOST: &str = "veyra.background";

pub fn folder(data_dir: &Path) -> PathBuf {
    data_dir.join("background")
}

/// Maps `https://veyra.background/` to the background folder.
pub fn install(webview: &ICoreWebView2, data_dir: &Path) -> windows::core::Result<()> {
    let dir = folder(data_dir);
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        webview.cast::<ICoreWebView2_3>()?.SetVirtualHostNameToFolderMapping(
            &HSTRING::from(HOST),
            &HSTRING::from(dir.as_os_str()),
            COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_ALLOW,
        )
    }
}

/// A name nobody can guess (64 random bits from the OS-seeded hasher).
fn random_name(ext: &str) -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("bg-{:016x}.{ext}", h.finish())
}

/// The Windows "Open" dialog for an image or a video.
pub fn pick_file(hwnd: HWND) -> Option<PathBuf> {
    use crate::i18n::t;
    use windows::Win32::{
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        UI::Shell::{Common::COMDLG_FILTERSPEC, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH},
    };
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dlg.SetOptions(FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dlg.SetTitle(&HSTRING::from(t("Sfondo della Nuova scheda", "New tab background")));
        let label = HSTRING::from(t("Immagini e video", "Images and videos"));
        let spec = w!("*.jpg;*.jpeg;*.png;*.webp;*.gif;*.avif;*.bmp;*.mp4;*.m4v;*.webm;*.mov");
        let _ = dlg.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: PCWSTR(label.as_ptr()), pszSpec: spec }]);
        dlg.Show(Some(hwnd)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        Some(PathBuf::from(CoTaskMemPWSTR::from(p).to_string()))
    }
}

impl App {
    /// The user picked `path`: it becomes the background (copied in, the
    /// previous one removed).
    pub fn bg_picked(&mut self, path: PathBuf) {
        let name_of = |p: &Path| p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        let Some(ext) = name_of(&path).filter(|e| {
            matches!(e.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "bmp" | "mp4" | "m4v" | "webm" | "mov")
        }) else {
            return;
        };
        let dir = folder(&self.data_dir);
        let _ = std::fs::create_dir_all(&dir);
        let name = random_name(&ext);
        if std::fs::copy(&path, dir.join(&name)).is_err() {
            return;
        }
        self.clean_backgrounds(&name);
        let mut new = self.settings.clone();
        (new.new_tab_bg, new.new_tab_bg_file, new.new_tab_background) = ("image".into(), name, true);
        self.apply_settings(new);
        self.notify_internal_pages(None);
    }

    /// Only the current background stays in the folder.
    fn clean_backgrounds(&self, keep: &str) {
        if let Ok(rd) = std::fs::read_dir(folder(&self.data_dir)) {
            for e in rd.flatten() {
                if e.file_name().to_str() != Some(keep) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    /// 0.4.0 kept the user's image as `newtab-background.jpg`: move it in.
    pub fn migrate_background(&mut self) {
        let old = self.data_dir.join("newtab-background.jpg");
        if !old.is_file() {
            return;
        }
        if self.settings.new_tab_bg_file.is_empty() {
            let dir = folder(&self.data_dir);
            let _ = std::fs::create_dir_all(&dir);
            let name = random_name("jpg");
            if std::fs::rename(&old, dir.join(&name)).is_ok() {
                self.settings.new_tab_bg_file = name;
                self.settings.save(&self.data_dir);
            }
        } else {
            let _ = std::fs::remove_file(old);
        }
    }
}

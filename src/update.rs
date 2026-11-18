//! Updates from the latest GitHub release (Settings > About).
//!
//! Checking and downloading run on a worker thread with the system `curl`
//! (system certificates, no network library). The installer is checked
//! against the size and SHA-256 digest GitHub publishes, then run silently:
//! it closes Veyra, updates it and opens it again.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::WindowsAndMessaging::{PostMessageW, WM_APP},
};

/// Repository the updates come from.
pub const REPO: &str = "MooseDevPJS/veyra";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Posted by the worker when the status changed (pages refresh).
pub const WM_UPDATE: u32 = WM_APP + 12;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub asset_url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    /// Download progress 0..1.
    Downloading(Release, f32),
    /// Verified, being installed.
    Installing,
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Idle);

pub fn status() -> Status {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn set(hwnd: HWND, s: Status) {
    if let Ok(mut st) = STATUS.lock() {
        *st = s;
    }
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_UPDATE, WPARAM(0), LPARAM(0));
    }
}

/// What the settings page shows.
pub fn status_json() -> serde_json::Value {
    use serde_json::json;
    match status() {
        Status::Idle => json!({ "state": "idle" }),
        Status::Checking => json!({ "state": "checking" }),
        Status::UpToDate => json!({ "state": "uptodate" }),
        Status::Available(r) => json!({ "state": "available", "version": r.version }),
        Status::Downloading(r, p) => json!({ "state": "downloading", "version": r.version, "progress": p }),
        Status::Installing => json!({ "state": "installing" }),
        Status::Failed(e) => json!({ "state": "failed", "error": e }),
    }
}

fn busy() -> bool {
    matches!(status(), Status::Checking | Status::Downloading(..) | Status::Installing)
}

/// Looks for a newer version (window `hwnd` hears back).
pub fn check(hwnd: HWND) {
    if busy() {
        return;
    }
    set(hwnd, Status::Checking);
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let hwnd = HWND(h as *mut _);
        let s = match fetch_latest() {
            Ok(r) if is_newer(&r.version, CURRENT) => Status::Available(r),
            Ok(_) => Status::UpToDate,
            Err(e) => Status::Failed(e),
        };
        set(hwnd, s);
    });
}

/// Downloads and verifies the available version, then installs it.
pub fn update(hwnd: HWND) {
    let Status::Available(rel) = status() else { return };
    set(hwnd, Status::Downloading(rel.clone(), 0.0));
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let hwnd = HWND(h as *mut _);
        let result = download(&rel, |p| set(hwnd, Status::Downloading(rel.clone(), p)))
            .and_then(|path| install(&path).map_err(|e| e.to_string()));
        match result {
            // The installer closes Veyra and reopens the new version; the
            // windows close themselves meanwhile (main.rs, WM_UPDATE).
            Ok(()) => set(hwnd, Status::Installing),
            Err(e) => set(hwnd, Status::Failed(e)),
        }
    });
}

/// The system `curl`, without a console window.
fn curl() -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let mut c = Command::new(system.join("System32").join("curl.exe"));
    c.creation_flags(CREATE_NO_WINDOW);
    c.args(["--silent", "--show-error", "--fail", "--location", "--max-time", "600", "--user-agent", concat!("Veyra/", env!("CARGO_PKG_VERSION"))]);
    c
}

fn fetch_latest() -> Result<Release, String> {
    let t = crate::i18n::t;
    let out = curl()
        .args(["--header", "Accept: application/vnd.github+json"])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        eprintln!("update check: {}", String::from_utf8_lossy(&out.stderr).trim());
        return Err(t("Impossibile contattare GitHub", "Couldn't reach GitHub").into());
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    parse_release(&json).ok_or_else(|| t("Nessun installer in questa versione", "No installer in this release").into())
}

/// The version and its `-setup.exe`, from GitHub's answer (only from this repository).
pub fn parse_release(json: &serde_json::Value) -> Option<Release> {
    let version = json["tag_name"].as_str()?.trim_start_matches('v').to_owned();
    let asset = json["assets"].as_array()?.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("-setup.exe")))?;
    let asset_url = asset["browser_download_url"].as_str()?.to_owned();
    if !asset_url.starts_with(&format!("https://github.com/{REPO}/releases/download/")) {
        return None;
    }
    Some(Release {
        version,
        asset_url,
        size: asset["size"].as_u64().unwrap_or(0),
        sha256: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase),
    })
}

/// `remote` newer than `current`? (By numbers: 0.10.0 > 0.9.3.)
pub fn is_newer(remote: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v').split(['-', '+']).next().unwrap_or("").split('.').map(|n| n.parse().unwrap_or(0)).collect()
    };
    let (r, c) = (parts(remote), parts(current));
    for i in 0..r.len().max(c.len()) {
        let (a, b) = (r.get(i).copied().unwrap_or(0), c.get(i).copied().unwrap_or(0));
        if a != b {
            return a > b;
        }
    }
    false
}

fn download(rel: &Release, progress: impl Fn(f32)) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("Veyra-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = rel.asset_url.rsplit('/').next().filter(|n| !n.is_empty() && !n.contains(['\\', '/'])).unwrap_or("veyra-setup.exe");
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let mut child = curl().arg("--output").arg(&path).arg(&rel.asset_url).spawn().map_err(|e| e.to_string())?;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if rel.size > 0 {
            let done = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            progress((done as f32 / rel.size as f32).min(0.99));
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    if !status.success() {
        return Err(crate::i18n::t("Download non riuscito", "Download failed").into());
    }
    verify(&path, rel)?;
    progress(1.0);
    Ok(path)
}

/// Size and SHA-256 of the downloaded file.
fn verify(path: &Path, rel: &Release) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let bad = |what: &str| {
        let _ = std::fs::remove_file(path);
        Err(format!("{} ({what})", crate::i18n::t("Il file scaricato non è valido", "The downloaded file is not valid")))
    };
    if rel.size > 0 && bytes.len() as u64 != rel.size {
        return bad("size");
    }
    if let Some(expected) = &rel.sha256 {
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if &actual != expected {
            return bad("SHA-256");
        }
    }
    Ok(())
}

/// The installer, silent: it closes Veyra, updates it and opens it again.
fn install(path: &Path) -> std::io::Result<()> {
    Command::new(path).args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"]).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.6.1", "0.6.0"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(!is_newer("0.6.0", "0.6.0"));
        assert!(!is_newer("0.5.9", "0.6.0"));
    }

    #[test]
    fn only_this_repositorys_installer() {
        let json = serde_json::json!({
            "tag_name": "v0.7.0",
            "assets": [{"name": "Veyra-0.7.0-setup.exe", "browser_download_url": "https://github.com/MooseDevPJS/veyra/releases/download/v0.7.0/Veyra-0.7.0-setup.exe", "size": 42, "digest": "sha256:ABC"}]
        });
        let r = parse_release(&json).unwrap();
        assert_eq!((r.version.as_str(), r.size, r.sha256.as_deref()), ("0.7.0", 42, Some("abc")));
        let mut evil = json.clone();
        evil["assets"][0]["browser_download_url"] = "https://example.com/Veyra-0.7.0-setup.exe".into();
        assert!(parse_release(&evil).is_none());
    }
}

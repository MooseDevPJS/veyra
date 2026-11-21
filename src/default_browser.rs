//! Veyra as Windows' default browser.
//!
//! Windows does not let a program make itself the default (since Windows 10
//! the choice is the user's, kept in a protected `UserChoice` key). What a
//! browser can do, and what this does: register as a browser for the
//! current user (http/https links, .html files), then open Windows' "Default
//! apps" page on Veyra, where one click sets it. The installer writes the
//! same keys (installer/veyra.iss), so Veyra is listed right after install.

use windows::{
    Win32::{
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW,
        },
        UI::{
            Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

const APP: &str = "Veyra";
const URL_CLASS: &str = "VeyraURL";
const HTML_CLASS: &str = "VeyraHTML";
const CLIENT: &str = r"Software\Clients\StartMenuInternet\Veyra";

/// Writes a string value, creating the key (`name` None = default value).
fn set(path: &str, name: Option<&str>, value: &str) -> bool {
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let name = name.map(HSTRING::from);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            name.as_ref().map(|n| PCWSTR(n.as_ptr())).unwrap_or(PCWSTR::null()),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
        .is_ok()
    }
}

fn get(path: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buf = [0u16; 256];
        let mut size = (buf.len() * 2) as u32;
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(path), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
            .ok()
            .ok()?;
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// Registers this veyra.exe as a browser for the current user. True if
/// every key was written.
pub fn register() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = exe.to_string_lossy().into_owned();
    let open = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    let desc = crate::i18n::t("Browser leggero e senza AI", "A light browser with no AI");
    let mut ok = true;
    for (class, name) in [(URL_CLASS, "Veyra URL"), (HTML_CLASS, crate::i18n::t("Documento HTML", "HTML document"))] {
        let base = format!(r"Software\Classes\{class}");
        ok &= set(&base, None, name);
        ok &= set(&format!(r"{base}\DefaultIcon"), None, &icon);
        ok &= set(&format!(r"{base}\shell\open\command"), None, &open);
        ok &= set(&format!(r"{base}\Application"), Some("ApplicationName"), APP);
    }
    ok &= set(&format!(r"Software\Classes\{URL_CLASS}"), Some("URL Protocol"), "");
    ok &= set(CLIENT, None, APP);
    ok &= set(&format!(r"{CLIENT}\DefaultIcon"), None, &icon);
    ok &= set(&format!(r"{CLIENT}\shell\open\command"), None, &format!("\"{exe}\""));
    let caps = format!(r"{CLIENT}\Capabilities");
    ok &= set(&caps, Some("ApplicationName"), APP);
    ok &= set(&caps, Some("ApplicationDescription"), desc);
    ok &= set(&caps, Some("ApplicationIcon"), &icon);
    ok &= set(&format!(r"{caps}\StartMenu"), Some("StartMenuInternet"), APP);
    for scheme in ["http", "https"] {
        ok &= set(&format!(r"{caps}\URLAssociations"), Some(scheme), URL_CLASS);
    }
    for ext in [".htm", ".html", ".shtml", ".xhtml"] {
        ok &= set(&format!(r"{caps}\FileAssociations"), Some(ext), HTML_CLASS);
        // Offered in "Open with" for these files too.
        ok &= set(&format!(r"Software\Classes\{ext}\OpenWithProgids"), Some(HTML_CLASS), "");
    }
    ok &= set(r"Software\RegisteredApplications", Some(APP), &caps);
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    ok
}

/// Veyra opens https links now (the user's choice in Windows).
pub fn is_default() -> bool {
    ["https", "http"].iter().all(|s| {
        get(&format!(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\{s}\UserChoice"), "ProgId").as_deref() == Some(URL_CLASS)
    })
}

/// Windows Settings > Default apps, on Veyra's page (one click sets it).
pub fn open_windows_settings() {
    unsafe {
        let page = HSTRING::from(format!("ms-settings:defaultapps?registeredAppUser={APP}"));
        let r = ShellExecuteW(None, w!("open"), &page, None, None, SW_SHOWNORMAL);
        if r.0 as isize <= 32 {
            ShellExecuteW(None, w!("open"), w!("ms-settings:defaultapps"), None, None, SW_SHOWNORMAL);
        }
    }
}

/// A command-line argument as an address: a file path becomes file:///.
pub fn arg_to_url(arg: &str) -> String {
    let p = std::path::Path::new(arg);
    if p.is_absolute() && p.exists() {
        let s = arg.replace('\\', "/");
        let enc: String = s
            .chars()
            .map(|c| match c {
                ' ' => "%20".to_string(),
                '%' => "%25".to_string(),
                '#' => "%23".to_string(),
                '?' => "%3F".to_string(),
                c => c.to_string(),
            })
            .collect();
        format!("file:///{enc}")
    } else {
        arg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_file_urls() {
        let dir = std::env::temp_dir().join("veyra test #1");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("a b.html");
        let _ = std::fs::write(&f, "<p>x</p>");
        let url = arg_to_url(&f.to_string_lossy());
        assert!(url.starts_with("file:///") && url.ends_with("veyra%20test%20%231/a%20b.html"), "{url}");
        assert_eq!(arg_to_url("https://example.com/"), "https://example.com/");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

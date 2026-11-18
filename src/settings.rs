//! User settings, stored as JSON in the data folder. Every field has a
//! default, so old or partial files keep working when new options appear.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shortcuts::Bindings;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Background tab is suspended (no CPU) after this many seconds. 0 = never.
    pub suspend_after_secs: u64,
    /// Background tab is unloaded (RAM freed) after this many seconds. 0 = never.
    pub unload_after_secs: u64,
    /// Unload background tabs early when free system RAM drops below this (MB). 0 = off.
    pub low_memory_mb: u64,
    /// Never suspend or unload a tab that is playing audio.
    pub keep_audio_tabs: bool,
    pub adblock: bool,
    /// `{}` is replaced by the URL-encoded query.
    pub search_url: String,
    pub restore_session: bool,
    /// Interface language: "auto" (Windows), "it" or "en".
    pub language: String,
    /// "auto" (follow Windows), "light" or "dark".
    pub theme: String,
    pub show_nav_buttons: bool,
    /// Windows' classic title bar on top (window buttons leave the sidebar).
    pub title_bar: bool,
    /// Ctrl+T opens a new tab page instead of the floating command bar.
    pub new_tab_page: bool,
    /// The new tab page shows a background (`new_tab_bg`).
    pub new_tab_background: bool,
    /// "image" (the user's own file, image or video: `new_tab_bg_file`),
    /// "color:N" or "gradient:N" (lists in ui/backgrounds.js).
    pub new_tab_bg: String,
    /// The user's background, a file name in `<data>/background/` (newtab.rs).
    pub new_tab_bg_file: String,
    /// Name, logo and search box color: "auto" (from the background), "light" or "dark".
    pub new_tab_text: String,
    /// Logo and search box: "top", "middle" or "bottom".
    pub new_tab_position: String,
    pub new_tab_logo: bool,
    pub new_tab_name: bool,
    /// Frosted glass (Acrylic) behind the sidebar; false = solid frame.
    pub glass: bool,
    /// Sidebar: "docked", "compact" (site icons only, expands over the page
    /// on hover) or "hidden" (revealed from the left edge).
    pub sidebar: String,
    /// Old on/off setting, read only to migrate older files to `sidebar`.
    #[serde(skip_serializing)]
    pub sidebar_visible: bool,
    /// Sidebar width in DIPs (200–400).
    pub sidebar_width: f32,
    /// Docked DevTools pane width in DIPs.
    pub devtools_width: f32,
    /// Tracking prevention: "strict", "balanced", "basic" or "off".
    pub tracking_prevention: String,
    /// Let the built-in engine offer to save passwords (off: use a manager).
    pub save_passwords: bool,
    /// Autofill of addresses and other form data.
    pub autofill: bool,
    /// Chromium's low-end device mode: ~7% less RAM, slightly less smooth.
    /// On by default on PCs with less than 8 GB.
    pub low_end_mode: bool,
    /// Extra Chromium command-line flags (advanced).
    pub browser_flags: String,
    pub shortcuts: Bindings,
}

impl Default for Settings {
    fn default() -> Self {
        let low_end = crate::sys::total_ram_mb() < 8 * 1024;
        Self {
            suspend_after_secs: if low_end { 60 } else { 5 * 60 },
            unload_after_secs: if low_end { 10 * 60 } else { 30 * 60 },
            low_memory_mb: if low_end { 700 } else { 1500 },
            keep_audio_tabs: true,
            adblock: true,
            // DuckDuckGo with every AI feature off (no Duck.ai / Search Assist).
            search_url: "https://noai.duckduckgo.com/?q={}".into(),
            restore_session: true,
            language: "auto".into(),
            theme: "auto".into(),
            show_nav_buttons: true,
            title_bar: false,
            new_tab_page: false,
            new_tab_background: false,
            new_tab_bg: "gradient:1".into(),
            new_tab_bg_file: String::new(),
            new_tab_text: "auto".into(),
            new_tab_position: "middle".into(),
            new_tab_logo: true,
            new_tab_name: true,
            glass: true,
            sidebar: String::new(), // resolved by sanitize()
            sidebar_visible: true,
            sidebar_width: 260.0,
            devtools_width: 460.0,
            // Balanced: blocks known trackers without breaking sign-ins;
            // the adblock lists already remove most of the rest.
            tracking_prevention: "balanced".into(),
            save_passwords: false,
            autofill: true,
            low_end_mode: low_end,
            browser_flags: String::new(),
            shortcuts: Bindings::default(),
        }
    }
}

impl Settings {
    /// Fixes values a hand-edited file or the settings page could break.
    pub fn sanitize(&mut self) {
        self.sidebar_width = self.sidebar_width.clamp(200.0, 420.0);
        self.devtools_width = self.devtools_width.clamp(280.0, 1400.0);
        if !self.search_url.contains("{}") || !self.search_url.starts_with("http") {
            self.search_url = Settings::default().search_url;
        }
        if !matches!(self.sidebar.as_str(), "docked" | "compact" | "hidden") {
            self.sidebar = if self.sidebar_visible { "docked" } else { "hidden" }.into();
        }
        if !matches!(self.theme.as_str(), "auto" | "light" | "dark") {
            self.theme = "auto".into();
        }
        if !matches!(self.language.as_str(), "auto" | "it" | "en") {
            self.language = "auto".into();
        }
        let bg_ok = self.new_tab_bg == "image"
            || self.new_tab_bg.split_once(':').is_some_and(|(k, n)| matches!(k, "color" | "gradient") && n.parse::<u8>().is_ok());
        if !bg_ok {
            self.new_tab_bg = Settings::default().new_tab_bg;
        }
        if !matches!(self.new_tab_text.as_str(), "auto" | "light" | "dark") {
            self.new_tab_text = "auto".into();
        }
        if !matches!(self.new_tab_position.as_str(), "top" | "middle" | "bottom") {
            self.new_tab_position = "middle".into();
        }
        // A bare file name in the background folder.
        if self.new_tab_bg_file.contains(['/', '\\']) || self.new_tab_bg_file.contains("..") {
            self.new_tab_bg_file.clear();
        }
    }

    /// Light or dark, resolving "auto" against the Windows setting.
    pub fn dark(&self, system_dark: bool) -> bool {
        match self.theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => system_dark,
        }
    }

    /// Options that only take effect after a restart.
    pub fn restart_differs(&self, other: &Settings) -> bool {
        self.adblock != other.adblock || self.low_end_mode != other.low_end_mode || self.browser_flags != other.browser_flags
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    /// Loads settings; a missing or broken file yields defaults (a broken file
    /// is kept aside as `settings.json.bad` so the user's edits are not lost).
    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        let mut s = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("settings.json invalid ({e}), using defaults");
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        s.sanitize();
        s
    }

    pub fn save(&self, data_dir: &Path) {
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).expect("settings serialize");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Turns what the user typed into a URL: keeps URLs, adds https:// to
    /// bare domains, sends everything else to the search engine.
    pub fn resolve_input(&self, input: &str) -> String {
        let s = input.trim();
        if s.contains("://") || s.starts_with("about:") {
            s.to_string()
        } else if is_address(s) {
            format!("https://{}", s.replace(' ', "%20"))
        } else {
            self.search_url.replace("{}", &url_encode(s))
        }
    }
}

/// True when typed text is an address rather than a search. A path may
/// contain spaces ("site.com/a b"); the host part may not.
pub fn is_address(input: &str) -> bool {
    let s = input.trim();
    if s.contains("://") || s.starts_with("about:") {
        return true;
    }
    let host = s.split('/').next().unwrap_or("");
    let has_path = s.len() > host.len();
    !host.contains(' ')
        && (!s.contains(' ') || has_path)
        && (host.starts_with("localhost") || (host.contains('.') && !host.ends_with('.')))
}

/// Inverse of `url_encode` (also accepts `+` for space).
pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_input() {
        let s = Settings { search_url: "https://s.test/?q={}".into(), ..Settings::default() };
        assert_eq!(s.resolve_input("youtube.com"), "https://youtube.com");
        assert_eq!(s.resolve_input("http://a.b/c"), "http://a.b/c");
        assert_eq!(s.resolve_input("rust lang"), "https://s.test/?q=rust+lang");
        assert_eq!(s.resolve_input("c++"), "https://s.test/?q=c%2B%2B");
        assert_eq!(s.resolve_input("a.com/?q=x y"), "https://a.com/?q=x%20y");
        assert_eq!(s.resolve_input("node.js tutorial"), "https://s.test/?q=node.js+tutorial");
        assert_eq!(s.resolve_input("end."), "https://s.test/?q=end.");
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let s: Settings = serde_json::from_str(r#"{"adblock": false}"#).unwrap();
        assert!(!s.adblock);
        assert!(s.restore_session);
    }
}

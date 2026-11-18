//! Wrapper around adblock-rust (Brave's engine): loads filter lists and uBO
//! resources, answers "block this request?" and "what to hide on this page?".
//! WebView2 wiring lives in `shield.rs`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use adblock::{
    Engine,
    lists::{FilterSet, ParseOptions},
    request::Request,
    resources::{PermissionMask, Resource},
};
use base64::{Engine as _, prelude::BASE64_STANDARD};

/// (file name, URL, trusted). Trusted lists may use scriptlets that need
/// extra permissions (uBO's own lists, as in uBlock Origin itself).
const LISTS: &[(&str, &str, bool)] = &[
    ("easylist.txt", "https://easylist.to/easylist/easylist.txt", false),
    ("easyprivacy.txt", "https://easylist.to/easylist/easyprivacy.txt", false),
    ("ubo-filters.txt", "https://ublockorigin.github.io/uAssets/filters/filters.txt", true),
    ("ubo-privacy.txt", "https://ublockorigin.github.io/uAssets/filters/privacy.txt", true),
    ("ubo-quick-fixes.txt", "https://ublockorigin.github.io/uAssets/filters/quick-fixes.txt", true),
    ("ubo-unbreak.txt", "https://ublockorigin.github.io/uAssets/filters/unbreak.txt", true),
    // Scam, malware and fake-download sites: what most redirect ads lead to.
    ("ubo-badware.txt", "https://ublockorigin.github.io/uAssets/filters/badware.txt", true),
    // Ad and tracking servers (also on by default in uBlock Origin).
    ("peter-lowe.txt", "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=adblockplus&showintro=0&mimetype=plaintext", false),
];

/// Lists may pull in sub-lists with `!#include name` (uBO's main list is
/// mostly sub-lists). They are stored next to it as `<stem>--<name>`.
fn include_path(dir: &Path, list: &str, name: &str) -> PathBuf {
    let stem = list.trim_end_matches(".txt");
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    dir.join(format!("{stem}--{safe}"))
}

fn includes(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| l.strip_prefix("!#include ")).map(|n| n.trim().to_string()).collect()
}

/// `!#if` conditions as uBlock Origin evaluates them on desktop Chromium.
fn condition_true(cond: &str) -> bool {
    let atom = |a: &str| {
        let a = a.trim();
        let (neg, name) = match a.strip_prefix('!') {
            Some(n) => (true, n.trim()),
            None => (false, a),
        };
        matches!(name, "env_chromium" | "env_edge") != neg
    };
    if cond.contains("||") { cond.split("||").any(atom) } else { cond.split("&&").all(atom) }
}

/// Filter text ready for the engine: `!#if` blocks resolved, sub-lists
/// inlined, and `$popup` rules (which adblock-rust does not support) split
/// out as `$document` rules for the separate popup engine.
#[derive(Default)]
struct Prepared {
    main: String,
    popups: String,
}

fn prepare(text: &str, dir: &Path, list: &str, out: &mut Prepared, depth: u32) {
    let mut stack: Vec<bool> = Vec::new();
    for line in text.lines() {
        let l = line.trim_end();
        if let Some(cond) = l.strip_prefix("!#if ") {
            stack.push(condition_true(cond));
            continue;
        }
        if l == "!#else" {
            if let Some(top) = stack.last_mut() {
                *top = !*top;
            }
            continue;
        }
        if l == "!#endif" {
            stack.pop();
            continue;
        }
        if !stack.iter().all(|b| *b) {
            continue;
        }
        if let Some(name) = l.strip_prefix("!#include ") {
            if depth < 3
                && let Ok(sub) = std::fs::read_to_string(include_path(dir, list, name.trim()))
            {
                prepare(&sub, dir, list, out, depth + 1);
            }
            continue;
        }
        let (target, rule) = match popup_rule(l) {
            Some(rule) => (&mut out.popups, rule),
            None => (&mut out.main, l.to_string()),
        };
        target.push_str(&rule);
        target.push('\n');
    }
}

/// `||ads.example^$popup,third-party` -> `||ads.example^$document,third-party`.
fn popup_rule(line: &str) -> Option<String> {
    if line.starts_with('!') || ["##", "#@#", "#?#", "#$#", "#%#"].iter().any(|c| line.contains(c)) {
        return None;
    }
    let at = line.rfind('$')?;
    let (pattern, options) = (&line[..at], &line[at + 1..]);
    let opts: Vec<&str> = options.split(',').collect();
    if !opts.contains(&"popup") {
        return None;
    }
    let mut rule = format!("{pattern}$document");
    for o in opts.into_iter().filter(|o| *o != "popup") {
        rule.push(',');
        rule.push_str(o);
    }
    Some(rule)
}

const REFRESH_AFTER: Duration = Duration::from_secs(4 * 24 * 3600);

pub enum Verdict {
    Allow,
    Block,
    /// Serve this body instead (uBO "redirect" resources such as noop.js).
    Redirect { mime: String, body: Vec<u8> },
}

/// What a page needs injected at document creation.
#[derive(Default)]
pub struct PageCosmetics {
    /// Site-specific CSS hiding rules.
    pub css: String,
    /// Scriptlets (+js rules) as one script.
    pub scriptlets: String,
    /// Passed back when asking for generic class/id rules.
    pub exceptions: HashSet<String>,
    /// Site opted out of generic hiding (`$generichide`).
    pub generichide: bool,
}

/// Parsed filter lists, before the engine is built.
pub struct Lists {
    set: FilterSet,
    popup_set: FilterSet,
}

pub struct Blocker {
    engine: Engine,
    /// `$popup` rules only, checked when a page opens a window or tab.
    popups: Engine,
}

impl Blocker {
    /// Builds the engine from the lists on disk. On the very first run the
    /// lists are downloaded first; afterwards stale lists are refreshed in the
    /// background and picked up at the next start.
    /// Reads and parses the lists (no engine yet: that part is not `Send`,
    /// this one can run on another thread). See `from_lists`.
    pub fn read_lists(data_dir: &Path) -> Lists {
        let dir = data_dir.join("lists");
        let _ = std::fs::create_dir_all(&dir);
        let missing: Vec<_> = LISTS.iter().filter(|(f, ..)| !dir.join(f).exists()).collect();
        for (file, url, _) in missing {
            download_list(&dir, file, url);
        }
        // Sub-lists missing (first run, or a list that gained one): fetch now.
        for (file, url, _) in LISTS {
            let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            if includes(&text).iter().any(|n| !include_path(&dir, file, n).exists()) {
                download_includes(&dir, file, url, &text);
            }
        }
        {
            let stale: Vec<_> = LISTS
                .iter()
                .filter(|(f, ..)| is_stale(&dir.join(f)))
                .map(|(f, u, _)| (*f, *u))
                .collect();
            if !stale.is_empty() {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for (file, url) in stale {
                        download_list(&dir, file, url);
                    }
                });
            }
        }

        let t = Instant::now();
        let mut set = FilterSet::new(false);
        let mut popup_set = FilterSet::new(false);
        for (file, _, trusted) in LISTS {
            let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
            let opts = ParseOptions {
                permissions: PermissionMask::from_bits(if *trusted { 1 } else { 0 }),
                ..ParseOptions::default()
            };
            let mut p = Prepared::default();
            prepare(&text, &dir, file, &mut p, 0);
            set.add_filter_list(std::mem::take(&mut p.main), opts);
            popup_set.add_filter_list(std::mem::take(&mut p.popups), ParseOptions::default());
        }
        eprintln!("adblock: lists parsed in {:?}", t.elapsed());
        Lists { set, popup_set }
    }

    /// Builds the engine from `read_lists`.
    pub fn from_lists(Lists { set, popup_set }: Lists) -> Self {
        let t = Instant::now();
        let mut engine = Engine::new_with_filter_set(set);
        let popups = Engine::new_with_filter_set(popup_set);
        let resources = load_resources();
        let count = resources.len();
        engine.use_resources(resources);
        eprintln!("adblock: engine ready in {:?} ({count} resources)", t.elapsed());
        Blocker { engine, popups }
    }

    #[cfg(test)]
    fn from_rules(rules: &str) -> Self {
        let mut p = Prepared::default();
        prepare(rules, Path::new("."), "test.txt", &mut p, 0);
        let mut set = FilterSet::new(false);
        set.add_filter_list(std::mem::take(&mut p.main), ParseOptions::default());
        let mut popup_set = FilterSet::new(false);
        popup_set.add_filter_list(std::mem::take(&mut p.popups), ParseOptions::default());
        Blocker { engine: Engine::new_with_filter_set(set), popups: Engine::new_with_filter_set(popup_set) }
    }

    /// A page wants to open `url` in a new window/tab: block it?
    pub fn popup_blocked(&self, url: &str, opener: &str) -> bool {
        Self::blocks(&self.popups, url, opener) || Self::blocks(&self.engine, url, opener)
    }

    /// The tab is about to load `url` as its page, leaving `source`. Only
    /// explicit `$document` rules (scam/malware sites) and popup rules (a
    /// page sending you to an ad network: the "popunder" redirect) apply.
    pub fn navigation_blocked(&self, url: &str, source: &str) -> bool {
        Self::blocks(&self.engine, url, source) || (source.starts_with("http") && Self::blocks(&self.popups, url, source))
    }

    fn blocks(engine: &Engine, url: &str, source: &str) -> bool {
        let Ok(req) = Request::new(url, source, "document", "GET") else { return false };
        let r = engine.check_network_request(&req);
        r.filter.is_some() && (r.exception.is_none() || r.important)
    }

    /// `kind` is a Chromium-style type: "script", "image", "sub_frame", ...
    pub fn check(&self, url: &str, source: &str, kind: &str, method: &str) -> Verdict {
        let Ok(req) = Request::new(url, source, kind, method) else {
            return Verdict::Allow;
        };
        let r = self.engine.check_network_request(&req);
        if r.exception.is_some() && !r.important {
            return Verdict::Allow;
        }
        if let Some(data_url) = r.redirect.as_deref().and_then(parse_data_url) {
            return Verdict::Redirect { mime: data_url.0, body: data_url.1 };
        }
        if r.filter.is_some() || r.important {
            Verdict::Block
        } else {
            Verdict::Allow
        }
    }

    pub fn page_cosmetics(&self, url: &str) -> PageCosmetics {
        let r = self.engine.url_cosmetic_resources(url);
        PageCosmetics {
            css: hide_css(r.hide_selectors.iter()),
            scriptlets: r.injected_script,
            exceptions: r.exceptions,
            generichide: r.generichide,
        }
    }

    /// CSS for generic rules matching the classes/ids found on a page.
    pub fn generic_css(&self, classes: &[String], ids: &[String], exceptions: &HashSet<String>) -> String {
        hide_css(self.engine.hidden_class_id_selectors(classes, ids, exceptions).iter())
    }
}

/// One rule per selector, so one invalid selector only drops itself.
fn hide_css<'a>(selectors: impl Iterator<Item = &'a String>) -> String {
    selectors.map(|s| format!("{s}{{display:none!important}}\n")).collect()
}

fn parse_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let (mime, base64) = match meta.strip_suffix(";base64") {
        Some(m) => (m, true),
        None => (meta, false),
    };
    let body = if base64 { BASE64_STANDARD.decode(data).ok()? } else { data.as_bytes().to_vec() };
    Some((mime.to_string(), body))
}

fn load_resources() -> Vec<Resource> {
    let candidates: [PathBuf; 2] = [
        crate::sys::exe_dir().join("ubo-resources.json"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ubo-resources.json"),
    ];
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str(&text) {
                Ok(r) => return r,
                Err(e) => eprintln!("adblock: bad resources file {}: {e}", path.display()),
            }
        }
    }
    eprintln!("adblock: no ubo-resources.json, scriptlets disabled");
    Vec::new()
}

fn is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_none_or(|age| age > REFRESH_AFTER)
}

/// Downloads a list, then the sub-lists it includes.
fn download_list(dir: &Path, file: &str, url: &str) {
    download(url, &dir.join(file));
    if let Ok(text) = std::fs::read_to_string(dir.join(file)) {
        download_includes(dir, file, url, &text);
    }
}

fn download_includes(dir: &Path, file: &str, url: &str, text: &str) {
    let base = &url[..url.rfind('/').map_or(url.len(), |i| i + 1)];
    for name in includes(text) {
        if name.contains("..") || name.contains(':') {
            continue; // only plain names next to the list
        }
        download(&format!("{base}{name}"), &include_path(dir, file, &name));
    }
}

/// Downloads with the system curl into a temp file, replacing the target only
/// on success, so a failed update never loses a working list.
fn download(url: &str, path: &Path) {
    let tmp = path.with_extension("download");
    let ok = crate::sys::quiet_command("curl.exe")
        .args(["-sSfL", "--ssl-no-revoke", "--max-time", "30", "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .is_ok_and(|s| s.success());
    if ok && std::fs::rename(&tmp, path).is_ok() {
        eprintln!("adblock: updated {}", path.display());
    } else {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("adblock: could not download {url}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_excepts() {
        let b = Blocker::from_rules("||ads.example^\n@@||ads.example/ok.js");
        let page = "https://news.test/";
        assert!(matches!(b.check("https://ads.example/x.js", page, "script", "GET"), Verdict::Block));
        assert!(matches!(b.check("https://ads.example/ok.js", page, "script", "GET"), Verdict::Allow));
        assert!(matches!(b.check("https://cdn.test/app.js", page, "script", "GET"), Verdict::Allow));
    }

    #[test]
    fn popup_rules_and_conditions() {
        let b = Blocker::from_rules(
            "||popads.example^$popup,third-party\n||scam.example^$document\n!#if env_mobile\n||mobile.example^$popup\n!#endif",
        );
        let page = "https://news.test/";
        assert!(b.popup_blocked("https://popads.example/x", page));
        assert!(!b.popup_blocked("https://ok.example/", page));
        assert!(!b.popup_blocked("https://mobile.example/", page));
        assert!(b.navigation_blocked("https://scam.example/", ""));
        assert!(b.navigation_blocked("https://popads.example/x", page));
        assert!(!b.navigation_blocked("https://popads.example/x", ""));
        // A popup rule must not block the same server's normal requests.
        assert!(matches!(b.check("https://popads.example/a.js", page, "script", "GET"), Verdict::Allow));
    }

    #[test]
    fn data_url_parsing() {
        assert_eq!(parse_data_url("data:text/plain;base64,aGk="), Some(("text/plain".into(), b"hi".to_vec())));
        assert_eq!(parse_data_url("data:text/css,a"), Some(("text/css".into(), b"a".to_vec())));
        assert_eq!(parse_data_url("https://x"), None);
    }

    #[test]
    fn site_specific_css() {
        let b = Blocker::from_rules("news.test##.promo\n##.banner-ad");
        let c = b.page_cosmetics("https://news.test/a");
        assert!(c.css.contains(".promo{display:none!important}"));
        let generic = b.generic_css(&["banner-ad".into()], &[], &c.exceptions);
        assert!(generic.contains(".banner-ad"));
    }
}

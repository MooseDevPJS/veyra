//! History and bookmarks, kept in memory and saved as JSON. Small enough
//! (capped) that loading, searching and saving stay in the milliseconds.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const HISTORY_CAP: usize = 5000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visits: u32,
    /// Unix seconds.
    pub last: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
}

#[derive(Default)]
pub struct Store {
    dir: PathBuf,
    pub history: Vec<Visit>,
    pub bookmarks: Vec<Bookmark>,
    dirty: bool,
}

/// One row of the address-field suggestions.
#[derive(Clone, Debug, PartialEq)]
pub enum Suggestion {
    /// Switch to an already open tab.
    Tab { id: u64, title: String, url: String },
    Bookmark { title: String, url: String },
    History { title: String, url: String },
    /// Go to what was typed (looks like an address) or search for it.
    Go { input: String, is_search: bool },
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Store {
    pub fn load(dir: &Path) -> Self {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        Store {
            dir: dir.to_path_buf(),
            history: read("history.json").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default(),
            bookmarks: read("bookmarks.json").and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default(),
            dirty: false,
        }
    }

    pub fn save(&mut self) {
        if !self.dirty || self.dir.as_os_str().is_empty() {
            return;
        }
        for (name, json) in [
            ("history.json", serde_json::to_string(&self.history)),
            ("bookmarks.json", serde_json::to_string_pretty(&self.bookmarks)),
        ] {
            if let Ok(json) = json {
                let path = self.dir.join(name);
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
        self.dirty = false;
    }

    pub fn record_visit(&mut self, url: &str, at: u64) {
        if !url.starts_with("http") {
            return;
        }
        match self.history.iter_mut().find(|v| v.url == url) {
            Some(v) => {
                v.visits += 1;
                v.last = at;
            }
            None => self.history.push(Visit { url: url.into(), title: String::new(), visits: 1, last: at }),
        }
        if self.history.len() > HISTORY_CAP {
            // Drop the least useful entries.
            self.history.sort_by(|a, b| frecency(b, at).total_cmp(&frecency(a, at)));
            self.history.truncate(HISTORY_CAP * 9 / 10);
        }
        self.dirty = true;
    }

    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(v) = self.history.iter_mut().find(|v| v.url == url)
            && v.title != title
        {
            v.title = title.into();
            self.dirty = true;
        }
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    /// Adds or removes a bookmark; returns true if it is now bookmarked.
    pub fn toggle_bookmark(&mut self, url: &str, title: &str) -> bool {
        self.dirty = true;
        if let Some(i) = self.bookmarks.iter().position(|b| b.url == url) {
            self.bookmarks.remove(i);
            false
        } else {
            self.bookmarks.push(Bookmark { url: url.into(), title: title.into() });
            true
        }
    }

    /// Suggestions for `input`: open tabs first, then bookmarks, then
    /// history by frecency, and always a "go/search" row at the top.
    pub fn suggest(&self, input: &str, tabs: &[(u64, &str, &str)], looks_like_url: bool, at: u64) -> Vec<Suggestion> {
        let q = input.trim().to_lowercase();
        let mut out = vec![Suggestion::Go { input: input.trim().into(), is_search: !looks_like_url }];
        if q.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = q.split_whitespace().collect();
        let hit = |title: &str, url: &str| {
            let hay = format!("{} {}", title.to_lowercase(), strip_scheme(url).to_lowercase());
            words.iter().all(|w| hay.contains(w))
        };
        let mut seen: Vec<String> = Vec::new();
        for (id, title, url) in tabs {
            if hit(title, url) && out.len() < 4 {
                out.push(Suggestion::Tab { id: *id, title: title.to_string(), url: url.to_string() });
                seen.push(url.to_string());
            }
        }
        for b in &self.bookmarks {
            if hit(&b.title, &b.url) && !seen.contains(&b.url) && out.len() < 6 {
                out.push(Suggestion::Bookmark { title: b.title.clone(), url: b.url.clone() });
                seen.push(b.url.clone());
            }
        }
        let mut hist: Vec<&Visit> =
            self.history.iter().filter(|v| hit(&v.title, &v.url) && !seen.contains(&v.url)).collect();
        // Host-prefix matches ("you" -> youtube.com) rank above the rest.
        hist.sort_by(|a, b| {
            let pa = host_prefix(&a.url, &q);
            let pb = host_prefix(&b.url, &q);
            pb.cmp(&pa).then(frecency(b, at).total_cmp(&frecency(a, at)))
        });
        for v in hist.into_iter().take(8usize.saturating_sub(out.len())) {
            out.push(Suggestion::History { title: v.title.clone(), url: v.url.clone() });
        }
        out
    }

}

fn strip_scheme(url: &str) -> &str {
    let u = url.split("://").nth(1).unwrap_or(url);
    u.strip_prefix("www.").unwrap_or(u)
}

fn host_prefix(url: &str, q: &str) -> bool {
    strip_scheme(url).to_lowercase().starts_with(q)
}

/// Visits weighted by recency (half-life ~ one week).
fn frecency(v: &Visit, at: u64) -> f64 {
    let days = at.saturating_sub(v.last) as f64 / 86_400.0;
    v.visits as f64 * 0.5f64.powf(days / 7.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let mut s = Store::default();
        s.record_visit("https://www.youtube.com/", 1000);
        s.set_title("https://www.youtube.com/", "YouTube");
        s.record_visit("https://news.test/youth", 1000);
        s.set_title("https://news.test/youth", "Youth news");
        for _ in 0..5 {
            s.record_visit("https://news.test/youth", 1000);
        }
        s
    }

    #[test]
    fn host_prefix_beats_frequency() {
        let s = store();
        let r = s.suggest("you", &[], false, 1000);
        assert_eq!(r[0], Suggestion::Go { input: "you".into(), is_search: true });
        assert!(matches!(&r[1], Suggestion::History { url, .. } if url.contains("youtube")));
    }

    #[test]
    fn open_tabs_come_first_and_are_not_repeated() {
        let s = store();
        let r = s.suggest("youtube", &[(7, "YouTube", "https://www.youtube.com/")], false, 1000);
        assert!(matches!(r[1], Suggestion::Tab { id: 7, .. }));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn bookmarks_toggle() {
        let mut s = Store::default();
        assert!(s.toggle_bookmark("https://a.test/", "A"));
        assert!(s.is_bookmarked("https://a.test/"));
        assert!(!s.toggle_bookmark("https://a.test/", "A"));
    }

    #[test]
    fn all_words_must_match() {
        let s = store();
        assert_eq!(s.suggest("youth zzz", &[], false, 1000).len(), 1);
    }
}

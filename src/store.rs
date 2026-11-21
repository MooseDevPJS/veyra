//! History and bookmarks, kept in memory and saved as JSON. Small enough
//! (capped) that loading, searching and saving stay in the milliseconds.
//!
//! Two kinds of history: `history` has one row per page (visit count, for
//! the address-field suggestions); `log` has every single visit with its
//! time (the history panel). The log is appended to `visits.jsonl` as it
//! grows and rewritten only after a deletion or a cleanup.

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

/// One visit of the history panel.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    #[serde(rename = "u")]
    pub url: String,
    #[serde(rename = "t", default)]
    pub title: String,
    /// Unix seconds.
    #[serde(rename = "a")]
    pub at: u64,
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
    /// Every visit, oldest first.
    pub log: Vec<Entry>,
    /// `log[logged..]` is not in `visits.jsonl` yet.
    logged: usize,
    /// The whole log must be rewritten (something was deleted).
    rewrite: bool,
    /// Last `prune`: (time, days).
    pruned: (u64, u32),
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
    /// A Veyra command (shell.rs `COMMANDS`): "privata" opens a private window.
    Command(crate::shortcuts::Action),
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
            log: Vec::new(),
            logged: 0,
            rewrite: false,
            pruned: (0, 0),
            dirty: false,
        }
        .with_log(read("visits.jsonl").unwrap_or_default())
    }

    /// Visits from `visits.jsonl` (one JSON object per line; a broken line,
    /// say from a crash while writing, is skipped).
    fn with_log(mut self, text: String) -> Self {
        self.log = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        self.log.sort_by_key(|e| e.at);
        self.logged = self.log.len();
        self
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
        self.save_log();
    }

    fn save_log(&mut self) {
        if self.dir.as_os_str().is_empty() || (!self.rewrite && self.logged == self.log.len()) {
            return;
        }
        // Titles arrive after the visit: fill them in before writing.
        for e in &mut self.log[if self.rewrite { 0 } else { self.logged }..] {
            if e.title.is_empty()
                && let Some(v) = self.history.iter().find(|v| v.url == e.url)
            {
                e.title = v.title.clone();
            }
        }
        let lines = |entries: &[Entry]| -> String {
            entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect()
        };
        let path = self.dir.join("visits.jsonl");
        let ok = if self.rewrite {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, lines(&self.log)).is_ok() && std::fs::rename(&tmp, &path).is_ok()
        } else {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(lines(&self.log[self.logged..]).as_bytes()))
                .is_ok()
        };
        if ok {
            self.logged = self.log.len();
            self.rewrite = false;
        }
    }

    pub fn record_visit(&mut self, url: &str, at: u64) {
        if !url.starts_with("http") {
            return;
        }
        let title = self.history.iter().find(|v| v.url == url).map(|v| v.title.clone()).unwrap_or_default();
        self.log.push(Entry { url: url.into(), title, at });
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
        // The latest visit of this page, if not written yet.
        let logged = self.logged;
        if let Some(e) = self.log.iter_mut().skip(logged).rev().find(|e| e.url == url) {
            e.title = title.into();
        }
    }

    /// Visits for the history panel, newest first, whose title or address
    /// contains every word of `query`: `limit` of them after the first `skip`.
    pub fn visits(&self, query: &str, skip: usize, limit: usize) -> Vec<&Entry> {
        let q = query.trim().to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.log
            .iter()
            .rev()
            .filter(|e| {
                words.is_empty() || {
                    let hay = format!("{} {}", e.title.to_lowercase(), strip_scheme(&e.url).to_lowercase());
                    words.iter().all(|w| hay.contains(w))
                }
            })
            .skip(skip)
            .take(limit)
            .collect()
    }

    /// Deletes the visits matching `gone`; a page left with no visit also
    /// leaves the address-field suggestions.
    fn delete_where(&mut self, gone: impl Fn(&Entry) -> bool) {
        let before = self.log.len();
        let mut touched: Vec<String> = Vec::new();
        self.log.retain(|e| {
            let g = gone(e);
            if g && !touched.contains(&e.url) {
                touched.push(e.url.clone());
            }
            !g
        });
        if self.log.len() == before {
            return;
        }
        let log = &self.log;
        self.history.retain(|v| !touched.contains(&v.url) || log.iter().any(|e| e.url == v.url));
        self.logged = self.logged.min(self.log.len());
        self.rewrite = true;
        self.dirty = true;
    }

    /// Single visits (address and time), from the panel.
    pub fn delete_visits(&mut self, visits: &[(String, u64)]) {
        self.delete_where(|e| visits.iter().any(|(u, a)| *u == e.url && *a == e.at));
    }

    /// Visits in [from, to) (Unix seconds): a day, the last hour, everything.
    pub fn delete_range(&mut self, from: u64, to: u64) {
        self.delete_where(|e| e.at >= from && e.at < to);
        if from == 0 && to == u64::MAX && !self.history.is_empty() {
            self.history.clear();
            self.dirty = true;
        }
    }

    /// Keeps `days` of history (0 = keep everything): older visits go, and
    /// pages not visited since then leave the suggestions too. At most once
    /// an hour unless `days` changed (it runs on the housekeeping timer).
    pub fn prune(&mut self, days: u32, at: u64) {
        if days == 0 || (self.pruned.1 == days && at < self.pruned.0 + 3600) {
            return;
        }
        self.pruned = (at, days);
        let cutoff = at.saturating_sub(days as u64 * 86_400);
        self.delete_where(|e| e.at < cutoff);
        let before = self.history.len();
        self.history.retain(|v| v.last >= cutoff);
        if self.history.len() != before {
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
    fn visit_log_search_and_delete() {
        let mut s = Store::default();
        s.record_visit("https://a.test/", 100);
        s.set_title("https://a.test/", "Alpha");
        s.record_visit("https://b.test/", 200);
        s.record_visit("https://a.test/", 300);
        // Newest first, every visit, titles from the page.
        let all: Vec<(u64, &str)> = s.visits("", 0, 10).iter().map(|e| (e.at, e.title.as_str())).collect();
        assert_eq!(all, vec![(300, "Alpha"), (200, ""), (100, "Alpha")]);
        assert_eq!(s.visits("alpha", 0, 10).len(), 2);
        assert_eq!(s.visits("", 1, 10).len(), 2);
        // One visit of a: the page stays a suggestion; b's only visit: it goes.
        s.delete_visits(&[("https://a.test/".into(), 300), ("https://b.test/".into(), 200)]);
        assert_eq!(s.log.len(), 1);
        assert!(s.history.iter().any(|v| v.url == "https://a.test/"));
        assert!(!s.history.iter().any(|v| v.url == "https://b.test/"));
        // A day; everything.
        s.record_visit("https://c.test/", 90_000);
        s.delete_range(86_400, 172_800);
        assert_eq!(s.log.len(), 1);
        s.delete_range(0, u64::MAX);
        assert!(s.log.is_empty() && s.history.is_empty());
    }

    #[test]
    fn prune_keeps_the_last_days() {
        let mut s = Store::default();
        let day = 86_400;
        s.record_visit("https://old.test/", day);
        s.record_visit("https://new.test/", 150 * day);
        s.prune(100, 160 * day);
        assert_eq!(s.log.iter().map(|e| e.url.as_str()).collect::<Vec<_>>(), vec!["https://new.test/"]);
        assert_eq!(s.history.len(), 1);
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

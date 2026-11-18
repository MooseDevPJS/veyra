//! Single-line text input drawn with Direct2D (used by the command bar).
//! Editing follows Windows conventions: Ctrl+arrows/Backspace/Delete by
//! word, Shift extends the selection, Ctrl+A/C/X/V/Z, Home/End.

#[derive(Default, Clone)]
pub struct TextField {
    /// UTF-16 so caret indices match DirectWrite and the clipboard.
    pub text: Vec<u16>,
    pub caret: usize,
    /// Other end of the selection (== caret when nothing is selected).
    pub anchor: usize,
    undo: Vec<(Vec<u16>, usize)>,
}

pub enum Key {
    Left,
    Right,
    Home,
    End,
    Back,
    Delete,
}

impl TextField {
    pub fn set(&mut self, s: &str, select_all: bool) {
        self.text = s.encode_utf16().collect();
        self.caret = self.text.len();
        self.anchor = if select_all { 0 } else { self.caret };
        self.undo.clear();
    }

    pub fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        String::from_utf16_lossy(&self.text[a..b])
    }

    fn snapshot(&mut self) {
        self.undo.push((self.text.clone(), self.caret));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.text = t;
            self.caret = c.min(self.text.len());
            self.anchor = self.caret;
        }
    }

    /// Replaces the selection with `s`.
    pub fn insert(&mut self, s: &[u16]) {
        self.snapshot();
        let (a, b) = self.selection();
        let clean: Vec<u16> = s.iter().copied().filter(|c| *c >= 0x20 && *c != 0x7F).collect();
        self.text.splice(a..b, clean.iter().copied());
        self.caret = a + clean.len();
        self.anchor = self.caret;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    pub fn key(&mut self, key: Key, ctrl: bool, shift: bool) {
        let len = self.text.len();
        let target = match key {
            Key::Left if !ctrl && !shift && self.caret != self.anchor => self.selection().0,
            Key::Right if !ctrl && !shift && self.caret != self.anchor => self.selection().1,
            Key::Left if ctrl => word_boundary(&self.text, self.caret, true),
            Key::Right if ctrl => word_boundary(&self.text, self.caret, false),
            Key::Left => self.caret.saturating_sub(1),
            Key::Right => (self.caret + 1).min(len),
            Key::Home => 0,
            Key::End => len,
            Key::Back | Key::Delete => {
                let (a, b) = self.selection();
                let (from, to) = if a != b {
                    (a, b)
                } else if matches!(key, Key::Back) {
                    (if ctrl { word_boundary(&self.text, a, true) } else { a.saturating_sub(1) }, a)
                } else {
                    (a, if ctrl { word_boundary(&self.text, a, false) } else { (a + 1).min(len) })
                };
                if from != to {
                    self.snapshot();
                    self.text.drain(from..to);
                }
                self.caret = from;
                self.anchor = from;
                return;
            }
        };
        self.caret = target;
        if !shift {
            self.anchor = target;
        }
    }
}

#[derive(PartialEq)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: u16) -> Class {
    match char::from_u32(c as u32) {
        Some(ch) if ch.is_whitespace() => Class::Space,
        Some(ch) if ch.is_alphanumeric() || ch == '_' => Class::Word,
        _ => Class::Other,
    }
}

/// Next word edge from `i` (skips spaces first, then one run of same-class
/// characters), like Chrome's address bar.
pub fn word_boundary(text: &[u16], mut i: usize, back: bool) -> usize {
    let at = |i: usize| if back { text[i - 1] } else { text[i] };
    let more = |i: usize| if back { i > 0 } else { i < text.len() };
    let step = |i: usize| if back { i - 1 } else { i + 1 };
    while more(i) && class(at(i)) == Class::Space {
        i = step(i);
    }
    if more(i) {
        let kind = class(at(i));
        while more(i) && class(at(i)) == kind {
            i = step(i);
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(s: &str) -> TextField {
        let mut f = TextField::default();
        f.set(s, false);
        f
    }

    #[test]
    fn ctrl_backspace_deletes_a_word() {
        let mut f = field("https://youtube.com/watch");
        f.key(Key::Back, true, false);
        assert_eq!(f.string(), "https://youtube.com/");
        f.key(Key::Back, true, false);
        assert_eq!(f.string(), "https://youtube.com");
        let mut f = field("rust lang  ");
        f.key(Key::Back, true, false);
        assert_eq!(f.string(), "rust ");
    }

    #[test]
    fn typing_replaces_selection_and_undo_restores() {
        let mut f = field("hello");
        f.select_all();
        f.insert(&"ciao".encode_utf16().collect::<Vec<_>>());
        assert_eq!(f.string(), "ciao");
        f.undo();
        assert_eq!(f.string(), "hello");
    }

    #[test]
    fn shift_extends_selection() {
        let mut f = field("abc def");
        f.key(Key::Left, true, true);
        assert_eq!(f.selected_text(), "def");
        f.key(Key::Left, false, false);
        assert_eq!((f.caret, f.anchor), (4, 4));
    }

    #[test]
    fn control_chars_are_not_inserted() {
        let mut f = field("");
        f.insert(&[0x7F, b'a' as u16, 0x08]);
        assert_eq!(f.string(), "a");
    }
}

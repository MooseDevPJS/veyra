//! Keyboard shortcuts. Every action can be rebound in settings.json, e.g.
//! `"shortcuts": { "new-tab": ["Ctrl+T"], "close-tab": ["Ctrl+W", "Ctrl+F4"] }`.
//! An empty list disables the action.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    NewTab,
    CloseTab,
    ReopenClosedTab,
    History,
    PrivateWindow,
    NextTab,
    PrevTab,
    Tab1,
    Tab2,
    Tab3,
    Tab4,
    Tab5,
    Tab6,
    Tab7,
    Tab8,
    LastTab,
    Reload,
    HardReload,
    Back,
    Forward,
    CopyUrl,
    FocusAddress,
    CommandBar,
    ToggleSidebar,
    PinTab,
    Bookmark,
    Find,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ShowShortcuts,
    DevTools,
    Settings,
}

/// Action → key combos ("Ctrl+Shift+T"). Missing actions get their default.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(transparent)]
pub struct Bindings(pub BTreeMap<Action, Vec<String>>);

impl Default for Bindings {
    fn default() -> Self {
        use Action::*;
        let table: &[(Action, &[&str])] = &[
            (NewTab, &["Ctrl+T"]),
            (CloseTab, &["Ctrl+W", "Ctrl+F4"]),
            (ReopenClosedTab, &["Ctrl+Shift+T"]),
            (History, &["Ctrl+H"]),
            (PrivateWindow, &["Ctrl+Shift+N"]),
            (NextTab, &["Ctrl+Tab", "Ctrl+PageDown", "Alt+Down"]),
            (PrevTab, &["Ctrl+Shift+Tab", "Ctrl+PageUp", "Alt+Up"]),
            (Tab1, &["Ctrl+1"]),
            (Tab2, &["Ctrl+2"]),
            (Tab3, &["Ctrl+3"]),
            (Tab4, &["Ctrl+4"]),
            (Tab5, &["Ctrl+5"]),
            (Tab6, &["Ctrl+6"]),
            (Tab7, &["Ctrl+7"]),
            (Tab8, &["Ctrl+8"]),
            (LastTab, &["Ctrl+9"]),
            (Reload, &["Ctrl+R", "F5"]),
            (HardReload, &["Ctrl+Shift+R", "Ctrl+F5"]),
            (Back, &["Alt+Left"]),
            (Forward, &["Alt+Right"]),
            (CopyUrl, &["Ctrl+Shift+C"]),
            (FocusAddress, &["Ctrl+L", "F6"]),
            (CommandBar, &["Ctrl+K"]),
            (ToggleSidebar, &["Ctrl+S"]),
            (PinTab, &["Ctrl+D"]),
            (Bookmark, &["Ctrl+Shift+D"]),
            (Find, &["Ctrl+F"]),
            (ZoomIn, &["Ctrl+Plus"]),
            (ZoomOut, &["Ctrl+Minus"]),
            (ZoomReset, &["Ctrl+0"]),
            (ShowShortcuts, &["Ctrl+Slash"]),
            (DevTools, &["F12", "Ctrl+Shift+I"]),
            (Settings, &["Ctrl+Comma"]),
        ];
        Bindings(
            table
                .iter()
                .map(|(a, keys)| (*a, keys.iter().map(|k| k.to_string()).collect()))
                .collect(),
        )
    }
}

const CTRL: u8 = 1;
const SHIFT: u8 = 2;
const ALT: u8 = 4;

/// Fast lookup built from [`Bindings`] (user values over defaults).
pub struct Keymap(HashMap<(u8, u16), Action>);

impl Keymap {
    pub fn new(user: &Bindings) -> Self {
        let mut merged = Bindings::default().0;
        merged.extend(user.0.iter().map(|(a, k)| (*a, k.clone())));
        let mut map = HashMap::new();
        for (action, combos) in merged {
            for combo in combos {
                match parse_combo(&combo) {
                    Some(key) => {
                        map.insert(key, action);
                    }
                    None => eprintln!("shortcut ignored, cannot parse {combo:?}"),
                }
            }
        }
        Keymap(map)
    }

    /// Action for a key press, reading modifier state from the keyboard.
    pub fn lookup(&self, vk: u16) -> Option<Action> {
        let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
        let mut mods = 0;
        if down(VK_CONTROL) {
            mods |= CTRL;
        }
        if down(VK_SHIFT) {
            mods |= SHIFT;
        }
        if down(VK_MENU) {
            mods |= ALT;
        }
        self.0.get(&(mods, vk)).copied()
    }
}

fn parse_combo(combo: &str) -> Option<(u8, u16)> {
    let mut mods = 0;
    let mut key = None;
    for part in combo.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= CTRL,
            "shift" => mods |= SHIFT,
            "alt" => mods |= ALT,
            "" => return None,
            name => key = Some(vk_from_name(name)?),
        }
    }
    Some((mods, key?))
}

fn vk_from_name(name: &str) -> Option<u16> {
    let b = name.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_alphanumeric()) {
        return Some(b[0].to_ascii_uppercase() as u16);
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then(|| VK_F1.0 + n - 1);
    }
    let vk = match name {
        "tab" => VK_TAB,
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "space" => VK_SPACE,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        "home" => VK_HOME,
        "end" => VK_END,
        "delete" | "del" => VK_DELETE,
        "backspace" => VK_BACK,
        "plus" | "=" => VK_OEM_PLUS,
        "minus" | "-" => VK_OEM_MINUS,
        "slash" | "/" => VK_OEM_2,
        "comma" | "," => VK_OEM_COMMA,
        "period" | "." => VK_OEM_PERIOD,
        _ => return None,
    };
    Some(vk.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combos() {
        assert_eq!(parse_combo("Ctrl+Shift+T"), Some((CTRL | SHIFT, b'T' as u16)));
        assert_eq!(parse_combo("F12"), Some((0, VK_F12.0)));
        assert_eq!(parse_combo("Alt+Left"), Some((ALT, VK_LEFT.0)));
        assert_eq!(parse_combo("Ctrl+"), None);
        assert_eq!(parse_combo("Ctrl+Nope"), None);
    }

    #[test]
    fn user_binding_overrides_default() {
        let mut user = BTreeMap::new();
        user.insert(Action::NewTab, vec!["Ctrl+N".to_string()]);
        let km = Keymap::new(&Bindings(user));
        assert_eq!(km.0.get(&(CTRL, b'N' as u16)), Some(&Action::NewTab));
        assert_eq!(km.0.get(&(CTRL, b'T' as u16)), None);
    }
}

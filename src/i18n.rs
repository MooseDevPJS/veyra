//! Interface language. Strings are written in place as `t("it", "en")` so
//! both versions stay side by side and none can go missing.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// "auto" follows the Windows display language (Italian → it, else en).
pub fn set(language: &str) {
    let english = match language {
        "it" => false,
        "en" => true,
        _ => !system_is_italian(),
    };
    ENGLISH.store(english, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Code for web pages ("it" / "en").
pub fn code() -> &'static str {
    if english() { "en" } else { "it" }
}

pub fn t(it: &'static str, en: &'static str) -> &'static str {
    if english() { en } else { it }
}

fn system_is_italian() -> bool {
    use windows::Win32::Globalization::GetUserDefaultUILanguage;
    // Primary language id of Italian is 0x10.
    (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == 0x10
}

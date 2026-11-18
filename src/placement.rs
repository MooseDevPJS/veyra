//! The main window reopens where it was closed: same monitor, position,
//! size and maximized state (`window.json` in the data folder). A saved
//! place on a monitor that is no longer there is ignored.

use std::path::Path;

use serde::{Deserialize, Serialize};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect},
    UI::WindowsAndMessaging::{GetWindowPlacement, SW_HIDE, SW_SHOWMAXIMIZED, SetWindowPlacement, WINDOWPLACEMENT},
};

#[derive(Serialize, Deserialize)]
struct Saved {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    maximized: bool,
}

fn file(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("window.json")
}

/// Saves the window's place. `before_fullscreen` is the placement to keep
/// when a page is fullscreen (not the screen-covering one).
pub fn save(hwnd: HWND, data_dir: &Path, before_fullscreen: Option<WINDOWPLACEMENT>) {
    let wp = before_fullscreen.or_else(|| {
        let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe { GetWindowPlacement(hwnd, &mut wp) }.ok().map(|_| wp)
    });
    let Some(wp) = wp else { return };
    let r = wp.rcNormalPosition;
    let saved = Saved { left: r.left, top: r.top, right: r.right, bottom: r.bottom, maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 };
    if let Ok(json) = serde_json::to_string(&saved) {
        let _ = std::fs::write(file(data_dir), json);
    }
}

/// Puts the (still hidden) window back where it was. Returns true if it
/// should be shown maximized.
pub fn restore(hwnd: HWND, data_dir: &Path) -> bool {
    let Some(s) = std::fs::read_to_string(file(data_dir)).ok().and_then(|t| serde_json::from_str::<Saved>(&t).ok()) else {
        return false;
    };
    let r = RECT { left: s.left, top: s.top, right: s.right, bottom: s.bottom };
    let sane = r.right - r.left >= 400 && r.bottom - r.top >= 300;
    // Only on a monitor that still exists (a second screen may be unplugged).
    if !sane || unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONULL) }.is_invalid() {
        return false;
    }
    let wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: SW_HIDE.0 as u32,
        rcNormalPosition: r,
        ..Default::default()
    };
    unsafe {
        let _ = SetWindowPlacement(hwnd, &wp);
    }
    s.maximized
}

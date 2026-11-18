//! Small system queries.

use std::path::PathBuf;

use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

fn memory_status() -> MEMORYSTATUSEX {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        let _ = GlobalMemoryStatusEx(&mut m);
    }
    m
}

pub fn total_ram_mb() -> u64 {
    memory_status().ullTotalPhys / (1024 * 1024)
}

pub fn free_ram_mb() -> u64 {
    memory_status().ullAvailPhys / (1024 * 1024)
}

/// `%LOCALAPPDATA%\Browser` (profile, lists, settings).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    let dir = base.join("Veyra");
    // Before the name Veyra the folder was "Browser": move it over once
    // (tabs, settings, bookmarks, logins, extensions). If the old app still
    // runs and holds it, keep using it this time.
    let old = base.join("Browser");
    if !dir.exists() && old.join("settings.json").exists() && std::fs::rename(&old, &dir).is_err() {
        return old;
    }
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Folder of the running executable (bundled assets live next to it).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Into::into))
        .unwrap_or_else(|| ".".into())
}

/// Performance tracing, enabled with BROWSER_PERF=1 (stderr).
pub fn perf_on() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("BROWSER_PERF").is_ok_and(|v| v == "1"))
}

/// Logs `label` with the time since `start` when tracing is on and it took
/// at least `min_ms`.
pub fn perf(label: &str, start: std::time::Instant, min_ms: u128) {
    if perf_on() {
        let ms = start.elapsed().as_millis();
        if ms >= min_ms {
            eprintln!("[perf] {label}: {ms} ms");
        }
    }
}
/// A console program run without flashing a console window (the app is a
/// GUI program, so every child would otherwise get its own window).
pub fn quiet_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = std::process::Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

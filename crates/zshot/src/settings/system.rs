//! The bits of the general page that touch Windows rather than settings.json.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::error::{Error, Result};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Zshot";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `%LOCALAPPDATA%\Zshot` — settings.json and the log folder live under it.
pub fn data_dir() -> Result<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").map_err(|_| Error::new("LOCALAPPDATA is not set"))?;
    Ok(PathBuf::from(base).join("Zshot"))
}

/// The registry is the source of truth: the user can drop the entry from Task
/// Manager's startup tab, and a cached copy in settings.json would then lie.
pub fn autostart_enabled() -> bool {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(wide(RUN_KEY).as_ptr()),
            PCWSTR(wide(RUN_VALUE).as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut len),
        )
    };
    status.is_ok()
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let key = wide(RUN_KEY);
    let name = wide(RUN_VALUE);
    if !enabled {
        // Absent already is the desired state, not a failure.
        let _ = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(key.as_ptr()),
                PCWSTR(name.as_ptr()),
            )
        };
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    // Quoted: the path has spaces under "Program Files", and an unquoted Run
    // entry would launch the first token instead.
    let command = wide(&format!("\"{}\"", exe.display()));
    let bytes = command.len() * 2;
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(command.as_ptr() as *const c_void),
            bytes as u32,
        )
        .ok()?;
    }
    Ok(())
}

pub fn open_folder(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    let verb = wide("open");
    let target = wide(&path.display().to_string());
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a fake HINSTANCE; <= 32 is the error range.
    if result.0 as usize <= 32 {
        return Err(Error::new(format!("无法打开 {}", path.display())));
    }
    Ok(())
}

/// Deletes the files in the log folder, keeping the folder itself. Returns how
/// many went away so the page can say something more useful than "done".
pub fn clear_logs() -> Result<usize> {
    let dir = crate::perf::log_dir()?;
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        // A log the running process still holds open cannot be deleted on
        // Windows; skipping it is better than aborting the whole sweep.
        if path.is_file() && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

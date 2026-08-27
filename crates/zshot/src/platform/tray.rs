use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetCursorPos, InsertMenuW, LoadIconW, LoadImageW,
    RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, MF_BYPOSITION, MF_STRING, TPM_LEFTALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_LBUTTONUP, WM_RBUTTONUP,
};

use crate::error::Result;
use crate::platform::WM_TRAYICON;

const ID_REGION: u32 = 1;
const ID_FULL: u32 = 2;
const ID_COPY: u32 = 3;
const ID_EXIT: u32 = 9;

#[derive(Clone, Copy, Debug)]
pub enum TrayCommand {
    Region,
    Fullscreen,
    CopyOnly,
    Exit,
}

/// Explorer broadcasts this after a restart; every shell icon is gone by then and
/// has to be re-added, otherwise the tray entry disappears for the process lifetime.
pub fn taskbar_created_message() -> u32 {
    static MSG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MSG.get_or_init(|| unsafe {
        let name: Vec<u16> = "TaskbarCreated".encode_utf16().chain(std::iter::once(0)).collect();
        RegisterWindowMessageW(PCWSTR(name.as_ptr()))
    })
}

/// Embedded by `build.rs` as resource 1 (`winres` `set_icon`).
pub fn load_app_icon() -> Result<HICON> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let handle = LoadImageW(
            Some(module.into()),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        );
        match handle {
            Ok(h) => Ok(HICON(h.0)),
            Err(_) => Ok(LoadIconW(None, IDI_APPLICATION)?),
        }
    }
}

pub fn add_tray_icon(hwnd: HWND) -> Result<()> {
    unsafe {
        let icon = load_app_icon()?;
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        let tip: Vec<u16> = "Zshot\0".encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);

        let mut last_err = None;
        for attempt in 0..3 {
            match Shell_NotifyIconW(NIM_ADD, &nid).ok() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt + 1 < 3 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        }
        Err(last_err
            .expect("NIM_ADD retried with at least one error")
            .into())
    }
}

pub fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn handle_tray(hwnd: HWND, lparam: LPARAM) -> Option<TrayCommand> {
    let msg = lparam.0 as u32 & 0xFFFF;
    if msg == WM_RBUTTONUP {
        return show_menu(hwnd);
    }
    if msg == WM_LBUTTONUP {
        return Some(TrayCommand::Region);
    }
    None
}

fn show_menu(hwnd: HWND) -> Option<TrayCommand> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = InsertMenuW(menu, 0, MF_BYPOSITION | MF_STRING, ID_REGION as usize, PCWSTR(w("区域截图\0").as_ptr()));
        let _ = InsertMenuW(menu, 1, MF_BYPOSITION | MF_STRING, ID_FULL as usize, PCWSTR(w("全屏截图\0").as_ptr()));
        let _ = InsertMenuW(menu, 2, MF_BYPOSITION | MF_STRING, ID_COPY as usize, PCWSTR(w("仅复制\0").as_ptr()));
        let _ = InsertMenuW(menu, 3, MF_BYPOSITION | MF_STRING, ID_EXIT as usize, PCWSTR(w("退出\0").as_ptr()));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        match cmd.0 as u32 {
            ID_REGION => Some(TrayCommand::Region),
            ID_FULL => Some(TrayCommand::Fullscreen),
            ID_COPY => Some(TrayCommand::CopyOnly),
            ID_EXIT => Some(TrayCommand::Exit),
            _ => None,
        }
    }
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

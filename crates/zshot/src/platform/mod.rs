mod hook;
mod instance;
mod tray;

pub use hook::{
    action_from_wparam, install_ll_hook, register_fallback_hotkeys, set_hook_target,
    uninstall_ll_hook, unregister_fallback_hotkeys,
};
pub use instance::SingleInstance;
pub use tray::{add_tray_icon, handle_tray, remove_tray_icon, TrayCommand};

use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

pub fn set_dpi_aware() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub const WM_ZSHOT_HOTKEY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP;
pub const WM_TRAYICON: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const WM_ZSHOT_DISPATCH: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 33;

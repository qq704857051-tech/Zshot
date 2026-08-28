mod hook;
mod instance;
mod tray;

pub use hook::{
    action_from_wparam, action_wparam, apply_bindings, hook_target, install_ll_hook,
    register_fallback_hotkeys, set_hook_target, start_recording, stop_recording, uninstall_ll_hook,
    unpack_hotkey, unregister_fallback_hotkeys,
};
pub use instance::SingleInstance;
pub use tray::{
    add_tray_icon, handle_tray, load_app_icon, remove_tray_icon, taskbar_created_message,
    TrayCommand,
};

use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

pub fn set_dpi_aware() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// Carries a `HotkeyAction` in WPARAM. Posted (never sent) from the LL hook, the
/// RegisterHotKey fallback, and the tray menu, so the capture never runs inside a
/// hook callback or a TrackPopupMenu modal loop.
pub const WM_ZSHOT_HOTKEY: u32 = WM_APP;
pub const WM_TRAYICON: u32 = WM_APP + 1;
/// Posted from the tray menu so the settings window never opens inside the
/// TrackPopupMenu modal loop.
pub const WM_ZSHOT_SETTINGS: u32 = WM_APP + 2;
/// Posted by the recording hook with a packed `Hotkey` in WPARAM.
pub const WM_ZSHOT_HOTKEY_RECORDED: u32 = WM_APP + 3;
/// Overlay-interactive sentinel. The value is fixed by the vendored gpui patch
/// (`Window::on_app_ready_sentinel`) — changing it here alone breaks the T7 mark.
pub const WM_APP_READY_SENTINEL: u32 = WM_APP + 32;

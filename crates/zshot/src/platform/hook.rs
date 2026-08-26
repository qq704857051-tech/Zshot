use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, HOT_KEY_MODIFIERS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::hotkey::{decide, Hotkey, HotkeyAction, HotkeyBindings, KeyEvent, VK_SNAPSHOT};
use crate::platform::WM_ZSHOT_HOTKEY;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static BINDINGS: std::sync::Mutex<Option<HotkeyBindings>> = std::sync::Mutex::new(None);

pub fn set_hook_target(hwnd: HWND, bindings: HotkeyBindings) {
    TARGET.store(hwnd.0 as isize, Ordering::SeqCst);
    *BINDINGS.lock().unwrap() = Some(bindings);
}

pub fn install_ll_hook() -> windows::core::Result<HHOOK> {
    unsafe {
        let h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)?;
        HOOK.store(h.0 as isize, Ordering::SeqCst);
        Ok(h)
    }
}

pub fn uninstall_ll_hook() {
    let h = HOOK.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
        }
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let msg = wparam.0 as u32;
        let key_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let key_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        if key_up || key_down {
            let ev = KeyEvent {
                vk: info.vkCode as u16,
                ctrl: down(VK_CONTROL.0),
                alt: down(VK_MENU.0),
                shift: down(VK_SHIFT.0),
                win: down(VK_LWIN.0) || down(VK_RWIN.0),
                key_up,
            };
            if let Some(bindings) = BINDINGS.lock().ok().and_then(|g| *g) {
                let d = decide(&bindings, ev);
                if d.swallow {
                    if let Some(action) = d.action {
                        let hwnd = HWND(TARGET.load(Ordering::SeqCst) as *mut _);
                        let _ = PostMessageW(
                            Some(hwnd),
                            WM_ZSHOT_HOTKEY,
                            WPARAM(action_wparam(action)),
                            LPARAM(0),
                        );
                    }
                    return LRESULT(1);
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

pub fn action_wparam(action: HotkeyAction) -> usize {
    match action {
        HotkeyAction::Region => 1,
        HotkeyAction::CopyOnly => 2,
        HotkeyAction::Fullscreen => 3,
    }
}

pub fn action_from_wparam(wp: usize) -> Option<HotkeyAction> {
    match wp {
        1 => Some(HotkeyAction::Region),
        2 => Some(HotkeyAction::CopyOnly),
        3 => Some(HotkeyAction::Fullscreen),
        _ => None,
    }
}

/// Desktop fallback if the LL hook cannot be installed.
pub fn register_fallback_hotkeys(hwnd: HWND, bindings: &HotkeyBindings) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), 1);
        let _ = UnregisterHotKey(Some(hwnd), 2);
        let _ = UnregisterHotKey(Some(hwnd), 3);
        let _ = register_one(hwnd, 1, bindings.region);
        let _ = register_one(hwnd, 2, bindings.copy_only);
        let _ = register_one(hwnd, 3, bindings.fullscreen);
    }
}

fn register_one(hwnd: HWND, id: i32, hotkey: Hotkey) -> windows::core::Result<()> {
    let mut mods = MOD_NOREPEAT;
    if hotkey.ctrl {
        mods |= MOD_CONTROL;
    }
    if hotkey.alt {
        mods |= MOD_ALT;
    }
    if hotkey.shift {
        mods |= HOT_KEY_MODIFIERS(0x0004); // MOD_SHIFT
    }
    if hotkey.win {
        mods |= HOT_KEY_MODIFIERS(0x0008); // MOD_WIN
    }
    // PrtSc with no modifiers is valid.
    let vk = if hotkey.vk == 0 { VK_SNAPSHOT } else { hotkey.vk };
    unsafe { RegisterHotKey(Some(hwnd), id, mods, vk as u32) }
}

pub fn unregister_fallback_hotkeys(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), 1);
        let _ = UnregisterHotKey(Some(hwnd), 2);
        let _ = UnregisterHotKey(Some(hwnd), 3);
    }
}

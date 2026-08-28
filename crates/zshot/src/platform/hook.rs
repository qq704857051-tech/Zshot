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

use crate::hotkey::{
    decide, is_modifier_vk, vk_name, Hotkey, HotkeyAction, HotkeyBindings, KeyEvent, VK_SNAPSHOT,
};
use crate::platform::{WM_ZSHOT_HOTKEY, WM_ZSHOT_HOTKEY_RECORDED};

const VK_ESCAPE: u16 = 0x1B;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static HOOK: AtomicIsize = AtomicIsize::new(0);
static RECORD_HOOK: AtomicIsize = AtomicIsize::new(0);
static BINDINGS: std::sync::Mutex<Option<HotkeyBindings>> = std::sync::Mutex::new(None);

pub fn set_hook_target(hwnd: HWND, bindings: HotkeyBindings) {
    TARGET.store(hwnd.0 as isize, Ordering::SeqCst);
    *BINDINGS.lock().unwrap() = Some(bindings);
}

/// The hidden message window, so a rebind can re-apply itself without threading
/// the handle through the settings view.
pub fn hook_target() -> HWND {
    HWND(TARGET.load(Ordering::SeqCst) as *mut _)
}

/// Swaps the action hook for a recording hook. The action hook has to go, or
/// the combo being bound fires a screenshot; a plain suspend is not enough
/// because gpui produces no keystroke at all for PrtSc, so the recorder needs
/// raw virtual keys of its own.
pub fn start_recording() {
    // Clicking a second hotkey field while one is already recording must not
    // strand the first hook: overwriting RECORD_HOOK would leak the handle and
    // leave two recorders swallowing the same keys.
    unhook_recorder();
    uninstall_ll_hook();
    unsafe {
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(record_proc), None, 0) {
            Ok(h) => {
                RECORD_HOOK.store(h.0 as isize, Ordering::SeqCst);
            }
            Err(e) => {
                eprintln!("hotkey recording unavailable: {e}");
                let _ = install_ll_hook();
            }
        }
    }
}

/// Safe to call when not recording; the settings window calls it on cancel,
/// on close and on losing focus, and a stuck recorder would be far worse than
/// an extra no-op.
pub fn stop_recording() {
    if !unhook_recorder() {
        return;
    }
    let _ = install_ll_hook();
}

/// Returns whether a recorder was actually taken down.
fn unhook_recorder() -> bool {
    let h = RECORD_HOOK.swap(0, Ordering::SeqCst);
    if h == 0 {
        return false;
    }
    unsafe {
        let _ = UnhookWindowsHookEx(HHOOK(h as *mut _));
    }
    true
}

/// ponytail: swallows only the key it is about to bind, never modifiers and
/// never Escape. A recorder that swallowed everything would leave the keyboard
/// dead if the window ever got stuck in this mode.
unsafe extern "system" fn record_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        let key_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        let key_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let vk = info.vkCode as u16;
        let bindable = vk_name(vk).is_some() && !is_modifier_vk(vk) && vk != VK_ESCAPE;
        if bindable && (key_up || key_down) {
            // Same edge the matcher uses, so PrtSc — which usually reports only
            // key-up — is recordable at all.
            let complete = if vk == VK_SNAPSHOT { key_up } else { key_down };
            if complete {
                let hotkey = Hotkey::new(
                    down(VK_CONTROL.0),
                    down(VK_MENU.0),
                    down(VK_SHIFT.0),
                    down(VK_LWIN.0) || down(VK_RWIN.0),
                    vk,
                );
                unsafe {
                    let _ = PostMessageW(
                        Some(hook_target()),
                        WM_ZSHOT_HOTKEY_RECORDED,
                        WPARAM(pack_hotkey(hotkey)),
                        LPARAM(0),
                    );
                }
            }
            // Both edges: otherwise PrtSc still opens the Windows snip overlay
            // behind the settings window.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Re-applies a rebind to both paths at once. The LL hook reads `BINDINGS`, but
/// the RegisterHotKey fallback holds its own copy in the desktop's table, so
/// updating only one leaves the old shortcut live on the other.
pub fn apply_bindings(bindings: HotkeyBindings) {
    let hwnd = hook_target();
    set_hook_target(hwnd, bindings);
    register_fallback_hotkeys(hwnd, &bindings);
}

pub fn pack_hotkey(h: Hotkey) -> usize {
    let mods = (h.ctrl as usize) | (h.alt as usize) << 1 | (h.shift as usize) << 2 | (h.win as usize) << 3;
    (mods << 16) | h.vk as usize
}

pub fn unpack_hotkey(packed: usize) -> Hotkey {
    let mods = packed >> 16;
    Hotkey::new(
        mods & 1 != 0,
        mods & 2 != 0,
        mods & 4 != 0,
        mods & 8 != 0,
        (packed & 0xFFFF) as u16,
    )
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

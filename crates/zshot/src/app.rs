use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
};
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassExW, TranslateMessage, CS_HREDRAW, CS_VREDRAW, HWND_MESSAGE, MSG, WM_DESTROY,
    WM_HOTKEY, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::capture::{capture_for_action, crop_sdr_bgra};
use crate::codec::{set_clipboard_dib, write_png_file};
use crate::error::{Error, Result};
use crate::hotkey::{HotkeyAction, HotkeyBindings};
use crate::overlay::run_overlay;
use crate::perf::PerfSession;
use crate::platform::{
    action_from_wparam, add_tray_icon, handle_tray, install_ll_hook, register_fallback_hotkeys,
    remove_tray_icon, set_dpi_aware, set_hook_target, uninstall_ll_hook,
    unregister_fallback_hotkeys, SingleInstance, TrayCommand, WM_TRAYICON, WM_ZSHOT_HOTKEY,
};

static BUSY: AtomicBool = AtomicBool::new(false);



pub fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--once-capture") => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "shot.png".to_string());
            return once_capture(PathBuf::from(path));
        }
        Some("--self-test") => return self_test(),
        Some("--probe-gpui") => return crate::gpui_probe::run(),
        _ => {}
    }
    run_tray()
}

pub fn once_capture(path: PathBuf) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let mut perf = PerfSession::start();
    perf.mark("T0");
    perf.mark("T1");
    perf.mark("T2");
    let cap = capture_for_action(false, &mut perf)?;
    let (px, w, h, stride) = cap.gpu.read_bgra(&cap.sdr_display)?;
    write_png_file(&path, &px, w, h, stride)?;
    let _ = perf.write_log();
    println!("wrote {} ({}x{})", path.display(), w, h);
    Ok(())
}

fn self_test() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let mut perf = PerfSession::start();
    perf.mark("T0");
    let _instance = SingleInstance::acquire()?;
    let hwnd = create_message_window()?;
    let bindings = HotkeyBindings::poc_defaults();
    set_hook_target(hwnd, bindings);
    let hook_ok = install_ll_hook().is_ok();
    register_fallback_hotkeys(hwnd, &bindings);
    add_tray_icon(hwnd)?;
    perf.mark("T1");
    let _ = perf.write_log();
    println!("zshot-ready hook={hook_ok}");
    remove_tray_icon(hwnd);
    unregister_fallback_hotkeys(hwnd);
    uninstall_ll_hook();
    Ok(())
}

fn run_tray() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let mut perf_boot = PerfSession::start();
    perf_boot.mark("T0");
    let _instance = match SingleInstance::acquire() {
        Ok(i) => i,
        Err(_) => {
            eprintln!("Zshot is already running");
            return Ok(());
        }
    };
    let hwnd = create_message_window()?;
    let bindings = HotkeyBindings::poc_defaults();
    set_hook_target(hwnd, bindings);
    if let Err(e) = install_ll_hook() {
        eprintln!("LL hook failed ({e}); using RegisterHotKey fallback");
    }
    register_fallback_hotkeys(hwnd, &bindings);
    add_tray_icon(hwnd)?;
    perf_boot.mark("T1");
    let _ = perf_boot.write_log();
    let _ = bindings;

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    remove_tray_icon(hwnd);
    unregister_fallback_hotkeys(hwnd);
    uninstall_ll_hook();
    Ok(())
}

fn create_message_window() -> Result<HWND> {
    let name = wide("ZshotHidden\0");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hidden_proc),
        hInstance: unsafe { GetModuleHandleW(None)?.into() },
        lpszClassName: PCWSTR(name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        let _ = RegisterClassExW(&wc);
        CreateWindowExW(
            Default::default(),
            PCWSTR(name.as_ptr()),
            PCWSTR(wide("Zshot\0").as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            GetModuleHandleW(None).ok().map(Into::into),
            None,
        )
        .map_err(Into::into)
    }
}

unsafe extern "system" fn hidden_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_ZSHOT_HOTKEY {
        if let Some(action) = action_from_wparam(wparam.0) {
            let _ = on_action(action);
        }
        return LRESULT(0);
    }
    if msg == WM_HOTKEY {
        let action = match wparam.0 {
            1 => Some(HotkeyAction::Region),
            2 => Some(HotkeyAction::CopyOnly),
            3 => Some(HotkeyAction::Fullscreen),
            _ => None,
        };
        if let Some(a) = action {
            let _ = on_action(a);
        }
        return LRESULT(0);
    }
    if msg == WM_TRAYICON {
        if let Some(cmd) = handle_tray(hwnd, lparam) {
            match cmd {
                TrayCommand::Region => {
                    let _ = on_action(HotkeyAction::Region);
                }
                TrayCommand::Fullscreen => {
                    let _ = on_action(HotkeyAction::Fullscreen);
                }
                TrayCommand::CopyOnly => {
                    let _ = on_action(HotkeyAction::CopyOnly);
                }
                TrayCommand::Exit => {
                    remove_tray_icon(hwnd);
                    PostQuitMessage(0);
                }
            }
        }
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn on_action(action: HotkeyAction) -> Result<()> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| take_shot(action)));
    BUSY.store(false, Ordering::SeqCst);
    match result {
        Ok(r) => r,
        Err(_) => Err(Error::new("capture panicked")),
    }
}

fn take_shot(action: HotkeyAction) -> Result<()> {
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
    }
    let mut perf = PerfSession::start();
    perf.mark("T2");
    let region = matches!(action, HotkeyAction::Region);
    let cap = match capture_for_action(region, &mut perf) {
        Ok(c) => c,
        Err(e) => {
            unsafe {
                let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
            }
            eprintln!("capture failed: {e}");
            return Err(e);
        }
    };

    let export = match action {
        HotkeyAction::Fullscreen => Some((0, 0, cap.width, cap.height, true, true)),
        HotkeyAction::CopyOnly => Some((0, 0, cap.width, cap.height, false, true)),
        HotkeyAction::Region => {
            // Exclusive-fullscreen region: frame is already captured; force overlay.
            let overlay = run_overlay(&cap, &mut perf)?;
            if overlay.confirmed {
                Some((overlay.x, overlay.y, overlay.w, overlay.h, true, true))
            } else {
                None
            }
        }
    };

    if let Some((x, y, w, h, write_file, copy)) = export {
        let (px, cw, ch, stride) = crop_sdr_bgra(&cap, x, y, w, h)?;
        if write_file {
            let path = default_png_path()?;
            write_png_file(&path, &px, cw, ch, stride)?;
            println!("saved {}", path.display());
        }
        if copy {
            let _ = set_clipboard_dib(&px, cw, ch, stride);
        }
    }
    let _ = perf.write_log();
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
    }
    Ok(())
}

fn default_png_path() -> Result<PathBuf> {
    unsafe {
        let pw = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None)?;
        let pictures = pw.to_string().unwrap_or_else(|_| "Pictures".into());
        let dir = PathBuf::from(pictures).join("Zshot");
        std::fs::create_dir_all(&dir)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(dir.join(format!("zshot-{stamp}.png")))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

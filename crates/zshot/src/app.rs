use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use gpui::{App, Application};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, PostMessageW, PostQuitMessage, RegisterClassExW, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, SW_HIDE, WM_DESTROY, WM_HOTKEY, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::action_queue::ActionQueue;
use crate::capture::{
    capture_for_action, crop_sdr_bgra, drop_gpu_resources, CapturedDesktop, GpuShaders,
};
use crate::codec::{set_clipboard_dib, write_png_file};
use crate::error::{Error, Result};
use crate::gpui_bridge::open_smoke_overlay;
use crate::hotkey::{HotkeyAction, HotkeyBindings};
use crate::overlay::run_overlay;
use crate::perf::PerfSession;
use crate::platform::{
    action_from_wparam, add_tray_icon, handle_tray, install_ll_hook, register_fallback_hotkeys,
    remove_tray_icon, set_dpi_aware, set_hook_target, uninstall_ll_hook,
    unregister_fallback_hotkeys, SingleInstance, TrayCommand, WM_TRAYICON, WM_ZSHOT_DISPATCH,
    WM_ZSHOT_HOTKEY,
};

static BUSY: AtomicBool = AtomicBool::new(false);
static ACTION_TX: Mutex<Option<ActionQueue>> = Mutex::new(None);
static ACTION_RX: Mutex<Option<std::sync::mpsc::Receiver<HotkeyAction>>> = Mutex::new(None);

thread_local! {
    static GPUI_APP: RefCell<Option<gpui::AsyncApp>> = const { RefCell::new(None) };
}

fn bind_gpui_app(cx: &App) {
    GPUI_APP.with(|slot| {
        *slot.borrow_mut() = Some(cx.to_async());
    });
}

fn unbind_gpui_app() {
    GPUI_APP.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

fn with_gpui_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    GPUI_APP.with(|slot| {
        let app = slot.borrow().clone()?;
        app.update(f).ok()
    })
}

fn install_gpu_lifecycle(cx: &App) {
    bind_gpui_app(cx);
    cx.on_gpu_reset_begin(|| {
        drop_gpu_resources();
    });
}

fn capture_from_app(
    cx: &mut App,
    region: bool,
    perf: &mut PerfSession,
) -> Result<CapturedDesktop> {
    let generation = cx.gpu_generation();
    let overlay_luid = cx.adapter_luid().unwrap_or((0, 0));
    match cx.with_gpu_context(|device, ctx| {
        GpuShaders::get_or_compile(generation, device)
            .and_then(|shaders| capture_for_action(region, perf, device, ctx, &shaders, overlay_luid))
    }) {
        Some(r) => r,
        None => Err(Error::new("GPU context unavailable")),
    }
}

fn crop_from_app(
    cx: &mut App,
    cap: &CapturedDesktop,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32, u32)> {
    match cx.with_gpu_context(|_device, ctx| crop_sdr_bgra(cap, ctx, x, y, w, h)) {
        Some(r) => r,
        None => Err(Error::new("GPU context unavailable")),
    }
}

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
        Some("--overlay-smoke") => return overlay_smoke(),
        Some("--probe-gpui") => {
            eprintln!("--probe-gpui removed; GPUI is the main message pump");
            return Ok(());
        }
        _ => {}
    }
    run_tray()
}

fn overlay_smoke() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let boot_error = std::rc::Rc::new(std::cell::RefCell::new(None::<Error>));
    Application::new().run({
        let boot_error = boot_error.clone();
        move |cx: &mut App| {
            if let Err(e) = open_smoke_overlay(cx) {
                eprintln!("{e}");
                *boot_error.borrow_mut() = Some(e);
                cx.quit();
            }
        }
    });
    if let Some(e) = boot_error.borrow_mut().take() {
        return Err(e);
    }
    Ok(())
}

pub fn once_capture(path: PathBuf) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let outcome = std::rc::Rc::new(RefCell::new(None::<Result<(u32, u32)>>));
    Application::new().run({
        let outcome = outcome.clone();
        let path = path.clone();
        move |cx: &mut App| {
            install_gpu_lifecycle(cx);
            let mut perf = PerfSession::start();
            perf.mark("T0");
            perf.mark("T1");
            perf.mark("T2");
            let r = (|| {
                let cap = capture_from_app(cx, false, &mut perf)?;
                let (px, w, h, stride) = crop_from_app(cx, &cap, 0, 0, cap.width, cap.height)?;
                write_png_file(&path, &px, w, h, stride)?;
                let _ = perf.write_log();
                Ok((w, h))
            })();
            *outcome.borrow_mut() = Some(r);
            cx.quit();
        }
    });
    unbind_gpui_app();
    let result = outcome.borrow_mut().take();
    match result {
        Some(Ok((w, h))) => {
            println!("wrote {} ({}x{})", path.display(), w, h);
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err(Error::new("once-capture did not run")),
    }
}

fn self_test() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let mut perf = PerfSession::start();
    perf.mark("T0");
    let _instance = SingleInstance::acquire()?;
    let hwnd = create_hidden_overlapped_window()?;
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

    let cleanup_hwnd = std::rc::Rc::new(std::cell::RefCell::new(None::<HWND>));
    let boot_error = std::rc::Rc::new(std::cell::RefCell::new(None::<Error>));

    Application::new().run({
        let cleanup_hwnd = cleanup_hwnd.clone();
        let boot_error = boot_error.clone();
        move |cx: &mut App| {
            match boot_resident(cx, cleanup_hwnd) {
                Ok(()) => {
                    perf_boot.mark("T1");
                    let _ = perf_boot.write_log();
                }
                Err(e) => {
                    eprintln!("{e}");
                    *boot_error.borrow_mut() = Some(e);
                    cx.quit();
                }
            }
        }
    });

    if let Some(hwnd) = cleanup_hwnd.borrow_mut().take() {
        remove_tray_icon(hwnd);
        unregister_fallback_hotkeys(hwnd);
    }
    uninstall_ll_hook();
    clear_action_queue();

    unbind_gpui_app();

    if let Some(e) = boot_error.borrow_mut().take() {
        return Err(e);
    }
    Ok(())
}

fn boot_resident(
    cx: &mut App,
    cleanup_hwnd: std::rc::Rc<std::cell::RefCell<Option<HWND>>>,
) -> Result<()> {
    install_gpu_lifecycle(cx);
    let (queue, rx) = ActionQueue::new();
    *ACTION_TX.lock().unwrap() = Some(queue);
    *ACTION_RX.lock().unwrap() = Some(rx);

    let hwnd = create_hidden_overlapped_window()?;
    *cleanup_hwnd.borrow_mut() = Some(hwnd);

    let bindings = HotkeyBindings::poc_defaults();
    set_hook_target(hwnd, bindings);
    if let Err(e) = install_ll_hook() {
        eprintln!("LL hook failed ({e}); using RegisterHotKey fallback");
    }
    register_fallback_hotkeys(hwnd, &bindings);
    add_tray_icon(hwnd)?;
    Ok(())
}

fn clear_action_queue() {
    *ACTION_TX.lock().unwrap() = None;
    *ACTION_RX.lock().unwrap() = None;
}

fn enqueue_action(hwnd: HWND, action: HotkeyAction) {
    if let Ok(guard) = ACTION_TX.lock() {
        if let Some(queue) = guard.as_ref() {
            queue.send(action);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_ZSHOT_DISPATCH, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn drain_actions() {
    let mut guard = match ACTION_RX.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(rx) = guard.as_mut() else {
        return;
    };
    while let Ok(action) = rx.try_recv() {
        let _ = dispatch_action(action);
    }
}

fn create_hidden_overlapped_window() -> Result<HWND> {
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
        let hwnd = CreateWindowExW(
            Default::default(),
            PCWSTR(name.as_ptr()),
            PCWSTR(wide("Zshot\0").as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            GetModuleHandleW(None).ok().map(Into::into),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_HIDE);
        Ok(hwnd)
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
            enqueue_action(hwnd, action);
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
            enqueue_action(hwnd, a);
        }
        return LRESULT(0);
    }
    if msg == WM_TRAYICON {
        if let Some(cmd) = handle_tray(hwnd, lparam) {
            match cmd {
                TrayCommand::Region => enqueue_action(hwnd, HotkeyAction::Region),
                TrayCommand::Fullscreen => enqueue_action(hwnd, HotkeyAction::Fullscreen),
                TrayCommand::CopyOnly => enqueue_action(hwnd, HotkeyAction::CopyOnly),
                TrayCommand::Exit => {
                    remove_tray_icon(hwnd);
                    clear_action_queue();
                    PostQuitMessage(0);
                }
            }
        }
        return LRESULT(0);
    }
    if msg == WM_ZSHOT_DISPATCH {
        drain_actions();
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        clear_action_queue();
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn dispatch_action(action: HotkeyAction) -> Result<()> {
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
    let mut perf = PerfSession::start();
    perf.mark("T2");
    let region = matches!(action, HotkeyAction::Region);
    let cap = match with_gpui_app(|cx| capture_from_app(cx, region, &mut perf)) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            eprintln!("capture failed: {e}");
            return Err(e);
        }
        None => return Err(Error::new("GPUI app unavailable")),
    };

    let export = match action {
        HotkeyAction::Fullscreen => Some((0, 0, cap.width, cap.height, true, true)),
        HotkeyAction::CopyOnly => Some((0, 0, cap.width, cap.height, false, true)),
        HotkeyAction::Region => {
            let overlay = run_overlay(&cap, &mut perf)?;
            if overlay.confirmed {
                Some((overlay.x, overlay.y, overlay.w, overlay.h, true, true))
            } else {
                None
            }
        }
    };

    if let Some((x, y, w, h, write_file, copy)) = export {
        let (px, cw, ch, stride) = with_gpui_app(|cx| crop_from_app(cx, &cap, x, y, w, h))
            .unwrap_or_else(|| Err(Error::new("GPUI app unavailable")))?;
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

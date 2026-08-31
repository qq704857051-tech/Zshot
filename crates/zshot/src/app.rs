use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, Application, Timer};

fn gpui_app() -> Application {
    Application::new().with_assets(crate::overlay::ToolbarAssets)
}
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, PostMessageW, PostQuitMessage, RegisterClassExW, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, SW_HIDE, WM_DESTROY, WM_HOTKEY, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::capture::{
    capture_for_action, crop_sdr_bgra, drop_gpu_resources, prepare_capture, CapturedDesktop,
    GpuShaders,
};
use crate::codec::{set_clipboard_dib, write_png_file};
use crate::error::{Error, Result};
use crate::gpui_bridge::drop_freeze_swapchain;
use crate::hotkey::{HotkeyAction, HotkeyBindings};
use crate::overlay::composite::apply_annotations;
use crate::overlay::{show_overlay, OverlayResult};
use crate::perf::{write_perf_summary, PerfSession};
use crate::platform::{
    action_from_wparam, action_wparam, add_tray_icon, handle_tray, install_ll_hook, load_app_icon,
    register_fallback_hotkeys, remove_tray_icon, set_dpi_aware, set_hook_target,
    taskbar_created_message, uninstall_ll_hook, unpack_hotkey, unregister_fallback_hotkeys,
    SingleInstance, TrayCommand, WM_TRAYICON, WM_ZSHOT_HOTKEY, WM_ZSHOT_HOTKEY_RECORDED,
    WM_ZSHOT_SETTINGS,
};

static BUSY: AtomicBool = AtomicBool::new(false);

thread_local! {
    static GPUI_APP: RefCell<Option<gpui::AsyncApp>> = const { RefCell::new(None) };
    static BENCH_CTRL: RefCell<Option<BenchControl>> = const { RefCell::new(None) };
}

struct BenchControl {
    samples: Vec<u128>,
    remaining: usize,
    /// After each sample: continue warm loop, quit once-mode, or stress sample.
    kind: BenchKind,
    stress_n: usize,
    stress_done: usize,
    stress_ws10: Option<usize>,
    stress_ws100: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BenchKind {
    Once,
    Warm,
    Stress,
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
        drop_freeze_swapchain();
    });
}

fn capture_from_app(cx: &mut App, region: bool, perf: &mut PerfSession) -> Result<CapturedDesktop> {
    let generation = cx.gpu_generation();
    let overlay_luid = cx.adapter_luid().unwrap_or((0, 0));
    // Borrow GPU only long enough to clone the device and compile shaders — do not hold
    // with_gpu_context across the parallel WGC wait.
    let (device, shaders) = match cx.with_gpu_context(|device, _ctx| {
        GpuShaders::get_or_compile(generation, device).map(|shaders| (device.clone(), shaders))
    }) {
        Some(Ok(pair)) => pair,
        Some(Err(e)) => return Err(e),
        None => return Err(Error::new("GPU context unavailable")),
    };
    let prepared = prepare_capture(region, &device, &shaders, overlay_luid)?;
    match cx
        .with_gpu_context(|device, ctx| capture_for_action(prepared, perf, device, ctx, &shaders))
    {
        Some(r) => r,
        None => Err(Error::new("GPU context unavailable")),
    }
}

pub(crate) fn crop_from_app(
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

/// Release builds are Windows-subsystem, so a shell-launched process starts with
/// no standard handles and every `println!` goes nowhere. The CLI subcommands
/// document their output, so borrow the parent's console before producing any.
fn attach_parent_console() {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

pub fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        attach_parent_console();
    }
    match args.get(1).map(String::as_str) {
        Some("--once-capture") => {
            let path = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "shot.png".to_string());
            return once_capture(PathBuf::from(path));
        }
        Some("--self-test") => return self_test(),
        Some("--bench-once") => return bench_once(),
        Some("--bench-overlay") => {
            let mode = arg_value(&args, "--mode").unwrap_or_else(|| "warm".into());
            let n = arg_value(&args, "--n")
                .and_then(|s| s.parse().ok())
                .unwrap_or(30);
            return match mode.as_str() {
                "first" => bench_overlay_first(n),
                "warm" => bench_overlay_warm(n),
                other => Err(Error::new(format!("unknown --mode {other}"))),
            };
        }
        Some("--stress-overlay") => {
            let n = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100usize);
            return stress_overlay(n);
        }
        _ => {}
    }
    run_tray()
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

pub fn once_capture(path: PathBuf) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    let outcome = std::rc::Rc::new(RefCell::new(None::<Result<(u32, u32)>>));
    gpui_app().run({
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
    let cleanup_hwnd = Rc::new(RefCell::new(None::<HWND>));
    let boot_error = Rc::new(RefCell::new(None::<Error>));
    let ready = Rc::new(Cell::new(false));

    gpui_app().run({
        let cleanup_hwnd = cleanup_hwnd.clone();
        let boot_error = boot_error.clone();
        let ready = ready.clone();
        move |cx: &mut App| {
            let mut perf = PerfSession::start();
            perf.mark("T0");
            let result = (|| -> Result<()> {
                let _instance = SingleInstance::acquire()?;
                let hwnd = create_hidden_overlapped_window()?;
                *cleanup_hwnd.borrow_mut() = Some(hwnd);
                let bindings = HotkeyBindings::poc_defaults();
                set_hook_target(hwnd, bindings);
                let hook_ok = install_ll_hook().is_ok();
                register_fallback_hotkeys(hwnd, &bindings);
                let tray_ok = add_tray_icon(hwnd).is_ok();
                perf.mark("T1");
                let _ = perf.write_log();
                println!("zshot-ready hook={hook_ok} tray={tray_ok}");
                ready.set(true);
                Ok(())
            })();
            if let Err(e) = result {
                eprintln!("{e}");
                *boot_error.borrow_mut() = Some(e);
            }
            cx.quit();
        }
    });

    if let Some(hwnd) = cleanup_hwnd.borrow_mut().take() {
        remove_tray_icon(hwnd);
        unregister_fallback_hotkeys(hwnd);
    }
    uninstall_ll_hook();

    if let Some(e) = boot_error.borrow_mut().take() {
        return Err(e);
    }
    if !ready.get() {
        return Err(Error::new("self-test did not reach ready"));
    }
    Ok(())
}

fn bench_once() -> Result<()> {
    std::env::set_var("ZSHOT_BENCH", "1");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    BENCH_CTRL.with(|slot| {
        *slot.borrow_mut() = Some(BenchControl {
            samples: Vec::new(),
            remaining: 1,
            kind: BenchKind::Once,
            stress_n: 0,
            stress_done: 0,
            stress_ws10: None,
            stress_ws100: None,
        });
    });
    gpui_app().run(|cx: &mut App| {
        install_gpu_lifecycle(cx);
        start_bench_iteration(cx);
    });
    unbind_gpui_app();
    Ok(())
}

fn bench_overlay_first(n: usize) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| Error::new(e.to_string()))?;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        let out = Command::new(&exe)
            .env("ZSHOT_BENCH", "1")
            .arg("--bench-once")
            .output()
            .map_err(|e| Error::new(e.to_string()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(Error::new(format!(
                "bench-once #{i} failed status={:?}: {stderr}",
                out.status.code()
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let Some(ms) = parse_bench_sample(&stdout) else {
            return Err(Error::new(format!(
                "bench-once #{i} missing bench-sample line: {stdout}"
            )));
        };
        samples.push(ms);
        println!("first sample {}/{} t2_t7_ms={ms}", i + 1, n);
    }
    let path = write_perf_summary("first", &mut samples)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn bench_overlay_warm(n: usize) -> Result<()> {
    std::env::set_var("ZSHOT_BENCH", "1");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    BENCH_CTRL.with(|slot| {
        *slot.borrow_mut() = Some(BenchControl {
            samples: Vec::with_capacity(n),
            remaining: n,
            kind: BenchKind::Warm,
            stress_n: 0,
            stress_done: 0,
            stress_ws10: None,
            stress_ws100: None,
        });
    });
    gpui_app().run(|cx: &mut App| {
        install_gpu_lifecycle(cx);
        start_bench_iteration(cx);
    });
    unbind_gpui_app();
    let mut samples = BENCH_CTRL.with(|slot| {
        slot.borrow_mut()
            .take()
            .map(|c| c.samples)
            .unwrap_or_default()
    });
    if samples.is_empty() {
        return Err(Error::new("warm bench produced no samples"));
    }
    let path = write_perf_summary("warm", &mut samples)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn stress_overlay(n: usize) -> Result<()> {
    std::env::set_var("ZSHOT_BENCH", "1");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    BENCH_CTRL.with(|slot| {
        *slot.borrow_mut() = Some(BenchControl {
            samples: Vec::new(),
            remaining: n,
            kind: BenchKind::Stress,
            stress_n: n,
            stress_done: 0,
            stress_ws10: None,
            stress_ws100: None,
        });
    });
    gpui_app().run(|cx: &mut App| {
        install_gpu_lifecycle(cx);
        start_bench_iteration(cx);
    });
    unbind_gpui_app();
    let ctrl = BENCH_CTRL.with(|slot| slot.borrow_mut().take());
    let Some(ctrl) = ctrl else {
        return Err(Error::new("stress bench state missing"));
    };
    println!(
        "stress n={} ws10_bytes={:?} ws100_bytes={:?}",
        ctrl.stress_n, ctrl.stress_ws10, ctrl.stress_ws100
    );
    if let (Some(a), Some(b)) = (ctrl.stress_ws10, ctrl.stress_ws100) {
        let delta = b as i64 - a as i64;
        println!("stress ws_delta_10_to_100={delta}");
    }
    Ok(())
}

fn parse_bench_sample(stdout: &str) -> Option<u128> {
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("bench-sample t2_t7_ms=") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn start_bench_iteration(cx: &mut App) {
    let mut perf = PerfSession::start();
    perf.mark("T2");
    match capture_from_app(cx, true, &mut perf) {
        Ok(cap) => {
            let _ = show_overlay(cx, cap, &mut perf, false);
        }
        Err(e) => {
            eprintln!("bench capture failed: {e}");
            cx.quit();
        }
    }
}

fn private_working_set_bytes() -> Option<usize> {
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )
        .ok()?;
        Some(counters.PrivateUsage)
    }
}

fn on_bench_sample(cx: &mut App, ms: u128) {
    let mut quit = false;
    BENCH_CTRL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(ctrl) = guard.as_mut() else {
            quit = true;
            return;
        };
        println!("bench-sample t2_t7_ms={ms}");
        match ctrl.kind {
            BenchKind::Once => {
                ctrl.samples.push(ms);
                ctrl.remaining = 0;
                quit = true;
            }
            BenchKind::Warm => {
                ctrl.samples.push(ms);
                ctrl.remaining = ctrl.remaining.saturating_sub(1);
                if ctrl.remaining == 0 {
                    quit = true;
                }
            }
            BenchKind::Stress => {
                ctrl.stress_done += 1;
                let done = ctrl.stress_done;
                if done == 10 {
                    ctrl.stress_ws10 = private_working_set_bytes();
                }
                if done == 100 {
                    ctrl.stress_ws100 = private_working_set_bytes();
                }
                ctrl.remaining = ctrl.remaining.saturating_sub(1);
                if ctrl.remaining == 0 {
                    quit = true;
                }
            }
        }
    });
    if quit {
        cx.quit();
        return;
    }
    // Yield a frame so hide settles, then next iteration.
    cx.spawn(async move |cx| {
        Timer::after(Duration::from_millis(16)).await;
        let _ = cx.update(start_bench_iteration);
    })
    .detach();
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

    gpui_app().run({
        let cleanup_hwnd = cleanup_hwnd.clone();
        let boot_error = boot_error.clone();
        move |cx: &mut App| match boot_resident(cx, cleanup_hwnd) {
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
    });

    if let Some(hwnd) = cleanup_hwnd.borrow_mut().take() {
        remove_tray_icon(hwnd);
        unregister_fallback_hotkeys(hwnd);
    }
    uninstall_ll_hook();

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
    let hwnd = create_hidden_overlapped_window()?;
    *cleanup_hwnd.borrow_mut() = Some(hwnd);

    let bindings = HotkeyBindings::from_settings(&crate::config::get().hotkeys);
    set_hook_target(hwnd, bindings);
    if let Err(e) = install_ll_hook() {
        eprintln!("LL hook failed ({e}); using RegisterHotKey fallback");
    }
    register_fallback_hotkeys(hwnd, &bindings);
    add_tray_icon(hwnd)?;
    Ok(())
}

/// Never dispatch inline: this is reached from the LL hook chain (which Windows
/// times out) and from inside the tray menu's modal loop.
fn post_action(hwnd: HWND, action: HotkeyAction) {
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_ZSHOT_HOTKEY,
            WPARAM(action_wparam(action)),
            LPARAM(0),
        );
    }
}

fn create_hidden_overlapped_window() -> Result<HWND> {
    let name = wide("ZshotHidden\0");
    let icon = load_app_icon().unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hidden_proc),
        hInstance: unsafe { GetModuleHandleW(None)?.into() },
        hIcon: icon,
        hIconSm: icon,
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
            if let Err(e) = dispatch_action(action) {
                crate::perf::log_app(&format!("action {action:?} failed: {e}"));
            }
        }
        return LRESULT(0);
    }
    if msg == WM_HOTKEY {
        // RegisterHotKey ids mirror action_wparam.
        if let Some(action) = action_from_wparam(wparam.0) {
            post_action(hwnd, action);
        }
        return LRESULT(0);
    }
    if msg == WM_ZSHOT_HOTKEY_RECORDED {
        let hotkey = unpack_hotkey(wparam.0);
        with_gpui_app(move |cx| crate::settings::on_hotkey_recorded(cx, hotkey));
        return LRESULT(0);
    }
    if msg == WM_ZSHOT_SETTINGS {
        with_gpui_app(crate::settings::open);
        return LRESULT(0);
    }
    if msg == taskbar_created_message() {
        let _ = add_tray_icon(hwnd);
        return LRESULT(0);
    }
    if msg == WM_TRAYICON {
        if let Some(cmd) = handle_tray(hwnd, lparam) {
            match cmd {
                TrayCommand::Region => post_action(hwnd, HotkeyAction::Region),
                TrayCommand::Fullscreen => post_action(hwnd, HotkeyAction::Fullscreen),
                TrayCommand::CopyOnly => post_action(hwnd, HotkeyAction::CopyOnly),
                TrayCommand::Settings => unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_ZSHOT_SETTINGS, WPARAM(0), LPARAM(0));
                },
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

fn dispatch_action(action: HotkeyAction) -> Result<()> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    // Overlay flows clear BUSY from complete_region_shot once the user is done.
    match take_shot(action) {
        Ok(ShotFlow::OverlayOpen) => Ok(()),
        Ok(ShotFlow::Finished) => {
            BUSY.store(false, Ordering::SeqCst);
            Ok(())
        }
        Err(e) => {
            BUSY.store(false, Ordering::SeqCst);
            Err(e)
        }
    }
}

enum ShotFlow {
    Finished,
    OverlayOpen,
}

fn take_shot(action: HotkeyAction) -> Result<ShotFlow> {
    let mut perf = PerfSession::start();
    perf.mark("T2");
    // Copy-only freezes the desktop like a region capture; the overlay then
    // confirms the drag straight to the clipboard. Only fullscreen skips the
    // overlay.
    let region = !matches!(action, HotkeyAction::Fullscreen);
    let cap = match with_gpui_app(|cx| capture_from_app(cx, region, &mut perf)) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            crate::perf::log_app(&format!("capture failed: {e}"));
            return Err(e);
        }
        None => return Err(Error::new("GPUI app unavailable")),
    };

    match action {
        HotkeyAction::Region | HotkeyAction::CopyOnly => {
            // Capture is done; do not hold App across overlay interaction.
            let copy_only = matches!(action, HotkeyAction::CopyOnly);
            match with_gpui_app(|cx| show_overlay(cx, cap, &mut perf, copy_only)) {
                Some(_) => Ok(ShotFlow::OverlayOpen),
                None => Err(Error::new("GPUI app unavailable")),
            }
        }
        HotkeyAction::Fullscreen => {
            let (x, y, w, h) = (0, 0, cap.width, cap.height);
            let (px, cw, ch, stride) = with_gpui_app(|cx| crop_from_app(cx, &cap, x, y, w, h))
                .unwrap_or_else(|| Err(Error::new("GPUI app unavailable")))?;
            export_pixels(px, cw, ch, stride, true, true)?;
            let _ = perf.write_log();
            Ok(ShotFlow::Finished)
        }
    }
}

pub(crate) fn complete_region_shot(
    result: OverlayResult,
    cap: Option<CapturedDesktop>,
    perf: Option<PerfSession>,
    cx: &mut App,
) {
    let bench = crate::overlay::bench_auto_hide_enabled();
    if result.confirmed && !bench {
        if let Some(cap) = cap {
            export_annotated_region(cx, &cap, &result);
        }
    }
    let mut sample = None;
    if let Some(perf) = perf {
        if bench {
            sample = perf.delta_ms("T2", "T7");
            if sample.is_none() {
                eprintln!("bench: missing T2/T7 marks (T6=WaitForCommit T7=sentinel)");
            }
        }
        let _ = perf.write_log();
    }
    BUSY.store(false, Ordering::SeqCst);
    if bench {
        match sample {
            Some(ms) => on_bench_sample(cx, ms),
            None => cx.quit(),
        }
    }
}

pub(crate) fn export_annotated_region(cx: &mut App, cap: &CapturedDesktop, result: &OverlayResult) {
    match crop_from_app(cx, cap, result.x, result.y, result.w, result.h) {
        Ok((mut px, cw, ch, stride)) => {
            apply_annotations(
                &mut px,
                cw,
                ch,
                stride,
                result.x as f64,
                result.y as f64,
                &result.marks,
            );
            if let Err(e) = export_pixels(px, cw, ch, stride, result.write_file, result.copy) {
                crate::perf::log_app(&format!("export failed: {e}"));
            }
        }
        Err(e) => crate::perf::log_app(&format!("crop failed: {e}")),
    }
}

fn export_pixels(
    px: Vec<u8>,
    cw: u32,
    ch: u32,
    stride: u32,
    write_file: bool,
    copy: bool,
) -> Result<()> {
    // Full-screen 4K is ~33 MB; share it with the writer thread instead of cloning.
    let px = std::sync::Arc::new(px);
    if write_file {
        let path = default_png_path()?;
        let px_file = px.clone();
        std::thread::spawn(move || {
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(
                    None,
                    windows::Win32::System::Com::COINIT_MULTITHREADED,
                );
            }
            if let Err(e) = write_png_file(&path, &px_file, cw, ch, stride) {
                crate::perf::log_app(&format!("png write failed: {e}"));
            }
        });
    }
    if copy {
        // Not swallowed: copy-only exports have no PNG to prove they ran, so a
        // silent failure here reads as "the hotkey is dead".
        set_clipboard_dib(&px, cw, ch, stride).map_err(|e| {
            crate::perf::log_app(&format!("clipboard write failed: {e}"));
            e
        })?;
        crate::perf::log_app(&format!("clipboard: {cw}x{ch} copied"));
    }
    Ok(())
}

fn default_png_path() -> Result<PathBuf> {
    let dir = crate::config::screenshot_dir()?;
    std::fs::create_dir_all(&dir)?;
    // Local time, not UTC: the name is for the user reading their own folder.
    let t = unsafe { GetLocalTime() };
    let stem = format!(
        "Zshot_{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    Ok(unique_path(&dir, &stem))
}

/// Two shots inside the same second share a stem; a screenshot silently
/// overwriting the previous one is the worst way to lose one.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.png"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}_{n}.png")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_suffixes_only_on_collision() {
        let dir = std::env::temp_dir().join("zshot-unique-path-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let first = unique_path(&dir, "Zshot_20260828_143052");
        assert_eq!(first.file_name().unwrap(), "Zshot_20260828_143052.png");

        std::fs::write(&first, b"").expect("write");
        let second = unique_path(&dir, "Zshot_20260828_143052");
        assert_eq!(second.file_name().unwrap(), "Zshot_20260828_143052_2.png");

        std::fs::write(&second, b"").expect("write");
        let third = unique_path(&dir, "Zshot_20260828_143052");
        assert_eq!(third.file_name().unwrap(), "Zshot_20260828_143052_3.png");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

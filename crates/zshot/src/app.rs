use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Focusable, Timer, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, FOLDERID_Pictures, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, PostMessageW, PostQuitMessage, RegisterClassExW, ShowWindow,
    CS_HREDRAW, CS_VREDRAW, SW_HIDE, WM_DESTROY, WM_HOTKEY, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::action_queue::ActionQueue;
use crate::capture::{
    capture_for_action, crop_sdr_bgra, drop_gpu_resources, prepare_capture, CapturedDesktop,
    GpuShaders,
};
use crate::codec::{set_clipboard_dib, write_png_file};
use crate::error::{Error, Result};
use crate::gpui_bridge::{drop_freeze_swapchain, open_smoke_overlay};
use crate::hotkey::{HotkeyAction, HotkeyBindings};
use crate::overlay::{show_overlay, OverlayResult, TextInput};
use crate::perf::{write_perf_summary, PerfSession};
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

fn capture_from_app(
    cx: &mut App,
    region: bool,
    perf: &mut PerfSession,
) -> Result<CapturedDesktop> {
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
    match cx.with_gpu_context(|device, ctx| capture_for_action(prepared, perf, device, ctx, &shaders))
    {
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
        Some("--ime-probe") => return ime_probe(),
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
            let n = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(100usize);
            return stress_overlay(n);
        }
        Some("--probe-gpui") => {
            eprintln!("--probe-gpui removed; use --ime-probe");
            return Ok(());
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
    let cleanup_hwnd = Rc::new(RefCell::new(None::<HWND>));
    let boot_error = Rc::new(RefCell::new(None::<Error>));
    let ready = Rc::new(Cell::new(false));

    Application::new().run({
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

fn ime_probe() -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    set_dpi_aware();
    Application::new().run(|cx: &mut App| {
        let bounds = gpui::Bounds {
            origin: gpui::point(px(200.), px(200.)),
            size: size(px(420.), px(80.)),
        };
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: true,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Opaque,
                topmost: true,
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|cx| {
                    let input = cx.new(TextInput::new);
                    ImeProbeView { input }
                })
            },
        );
        println!("ime-probe ready (type in the field; close window to exit)");
    });
    Ok(())
}

struct ImeProbeView {
    input: gpui::Entity<TextInput>,
}

impl Render for ImeProbeView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .p_2()
            .bg(rgb(0x1e1e1e))
            .child(div().w_full().child(self.input.clone()))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.input.focus_handle(cx));
                }),
            )
    }
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
    Application::new().run(|cx: &mut App| {
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
    Application::new().run(|cx: &mut App| {
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
    Application::new().run(|cx: &mut App| {
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
            let _ = show_overlay(cx, cap, &mut perf);
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
    match result {
        Ok(Ok(ShotFlow::OverlayOpen)) => Ok(()),
        Ok(Ok(ShotFlow::Finished)) => {
            BUSY.store(false, Ordering::SeqCst);
            Ok(())
        }
        Ok(Err(e)) => {
            BUSY.store(false, Ordering::SeqCst);
            Err(e)
        }
        Err(_) => {
            BUSY.store(false, Ordering::SeqCst);
            Err(Error::new("capture panicked"))
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
    let region = matches!(action, HotkeyAction::Region);
    let cap = match with_gpui_app(|cx| capture_from_app(cx, region, &mut perf)) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            eprintln!("capture failed: {e}");
            return Err(e);
        }
        None => return Err(Error::new("GPUI app unavailable")),
    };

    match action {
        HotkeyAction::Region => {
            // Capture is done; do not hold App across overlay interaction.
            match with_gpui_app(|cx| show_overlay(cx, cap, &mut perf)) {
                Some(_) => Ok(ShotFlow::OverlayOpen),
                None => Err(Error::new("GPUI app unavailable")),
            }
        }
        HotkeyAction::Fullscreen | HotkeyAction::CopyOnly => {
            let write_file = matches!(action, HotkeyAction::Fullscreen);
            let (x, y, w, h) = (0, 0, cap.width, cap.height);
            let (px, cw, ch, stride) = with_gpui_app(|cx| crop_from_app(cx, &cap, x, y, w, h))
                .unwrap_or_else(|| Err(Error::new("GPUI app unavailable")))?;
            export_pixels(px, cw, ch, stride, write_file, true)?;
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
            match crop_from_app(cx, &cap, result.x, result.y, result.w, result.h) {
                Ok((px, cw, ch, stride)) => {
                    if let Err(e) = export_pixels(px, cw, ch, stride, true, true) {
                        eprintln!("export failed: {e}");
                    }
                }
                Err(e) => eprintln!("crop failed: {e}"),
            }
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

fn export_pixels(
    px: Vec<u8>,
    cw: u32,
    ch: u32,
    stride: u32,
    write_file: bool,
    copy: bool,
) -> Result<()> {
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
            match write_png_file(&path, &px_file, cw, ch, stride) {
                Ok(()) => println!("saved {}", path.display()),
                Err(e) => eprintln!("png write failed: {e}"),
            }
        });
    }
    if copy {
        let _ = set_clipboard_dib(&px, cw, ch, stride);
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

//! Topmost freeze overlay: dim, marching ants, window snap, 15×15 magnifier.

use std::time::Instant;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory, ID2D1Factory1,
    ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DASH_STYLE_CUSTOM, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_REGULAR,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC1,
    DXGI_PRESENT, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    DXGI_SCALING_STRETCH,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetMessageW, KillTimer, LoadCursorW, PeekMessageW, PostMessageW, RegisterClassExW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    GWLP_USERDATA, HWND_TOPMOST, IDC_CROSS, LWA_ALPHA, MSG, PM_REMOVE, SWP_SHOWWINDOW, SW_SHOW,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_QUIT,
    WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WM_USER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WDA_EXCLUDEFROMCAPTURE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible,
    GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW as WS_EX_TOOLWINDOW_STYLE,
};

use crate::capture::CapturedDesktop;
use crate::error::Result;
use crate::geom::{hit_test, move_rect, physical_source_rect, resize_rect, Point, Rect, SelectionHitKind};
use crate::perf::PerfSession;

const MAG_COUNT: f32 = 15.0;
const MAG_PIXEL: f32 = 10.0;
const MIN_RECT: f64 = 5.0;
const WM_OVERLAY_DONE: u32 = WM_USER + 1;
const OVERLAY_TIMER_ID: usize = 1;

/// C# RegionCaptureWindow: dash {5,5}, offset = elapsed * -15.
pub const ANTS_DASH: [f32; 2] = [5.0, 5.0];
pub const ANTS_SPEED: f32 = -15.0;

pub fn marching_ants_dash_offset(elapsed_secs: f32) -> f32 {
    elapsed_secs * ANTS_SPEED
}

pub struct OverlayResult {
    pub confirmed: bool,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

struct OverlayState {
    hwnd: HWND,
    _cap_origin_x: i32,
    _cap_origin_y: i32,
    width: u32,
    height: u32,
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    factory: ID2D1Factory1,
    freeze: ID2D1Bitmap1,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    phase: Phase,
    mouse: Point,
    down: bool,
    down_pt: Point,
    selection: Rect,
    from_drag: bool,
    has_sel: bool,
    hover: Option<Rect>,
    windows: Vec<Rect>,
    hit: SelectionHitKind,
    sel_at_press: Rect,
    start: Instant,
    first_present: bool,
    result: OverlayResult,
    done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Selecting,
    Active,
}

pub fn run_overlay(cap: &CapturedDesktop, perf: &mut PerfSession) -> Result<OverlayResult> {
    unsafe { run_overlay_inner(cap, perf) }
}

unsafe fn run_overlay_inner(cap: &CapturedDesktop, perf: &mut PerfSession) -> Result<OverlayResult> {
    let class = register_class()?;
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    let hwnd = CreateWindowExW(
        ex,
        PCWSTR(class.as_ptr()),
        PCWSTR(w("ZshotOverlay\0").as_ptr()),
        WS_POPUP,
        cap.origin_x,
        cap.origin_y,
        cap.width as i32,
        cap.height as i32,
        None,
        None,
        GetModuleHandleW(None).ok().map(Into::into),
        None,
    )?;
    let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        cap.origin_x,
        cap.origin_y,
        cap.width as i32,
        cap.height as i32,
        SWP_SHOWWINDOW,
    );
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);

    let (swap, dc, freeze, dwrite, text_format, factory) = create_d2d(hwnd, cap)?;
    let _ = SetTimer(Some(hwnd), OVERLAY_TIMER_ID, 16, None);
    let mut mouse = Point { x: 0.0, y: 0.0 };
    let mut pt = POINT::default();
    if GetCursorPos(&mut pt).is_ok() {
        mouse.x = (pt.x - cap.origin_x) as f64;
        mouse.y = (pt.y - cap.origin_y) as f64;
    }
    let windows = detect_windows(hwnd, cap.origin_x, cap.origin_y);
    let hover = hit_window(&windows, mouse);

    let state = Box::new(OverlayState {
        hwnd,
        _cap_origin_x: cap.origin_x,
        _cap_origin_y: cap.origin_y,
        width: cap.width,
        height: cap.height,
        swap,
        dc,
        factory,
        freeze,
        dwrite,
        text_format,
        phase: Phase::Selecting,
        mouse,
        down: false,
        down_pt: mouse,
        selection: Rect::new(0.0, 0.0, 0.0, 0.0),
        from_drag: false,
        has_sel: false,
        hover,
        windows,
        hit: SelectionHitKind::None,
        sel_at_press: Rect::new(0.0, 0.0, 0.0, 0.0),
        start: Instant::now(),
        first_present: false,
        result: OverlayResult {
            confirmed: false,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        },
        done: false,
    });
    let ptr = Box::into_raw(state);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

    present_now(ptr, perf);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, Some(hwnd), 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
        if (*ptr).done {
            break;
        }
    }
    let _ = KillTimer(Some(hwnd), OVERLAY_TIMER_ID);
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    let boxed = Box::from_raw(ptr);
    let result = boxed.result;
    let _ = DestroyWindow(hwnd);
    drain_queued_quit();
    Ok(result)
}

unsafe fn drain_queued_quit() {
    let mut msg = MSG::default();
    while PeekMessageW(&mut msg, None, WM_QUIT, WM_QUIT, PM_REMOVE).as_bool() {}
}

fn request_close(s: &mut OverlayState) {
    s.done = true;
    unsafe {
        let _ = PostMessageW(Some(s.hwnd), WM_OVERLAY_DONE, WPARAM(0), LPARAM(0));
    }
}

unsafe fn present_now(ptr: *mut OverlayState, perf: &mut PerfSession) {
    if ptr.is_null() {
        return;
    }
    if let Err(e) = draw(&mut *ptr) {
        eprintln!("overlay draw: {e}");
        return;
    }
    if !(*ptr).first_present {
        (*ptr).first_present = true;
        perf.mark("T6");
        perf.mark("T7");
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut OverlayState;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let s = &mut *ptr;
    match msg {
        WM_SETCURSOR => {
            let _ = SetCursor(LoadCursorW(None, IDC_CROSS).ok());
            return LRESULT(1);
        }
        WM_MOUSEMOVE => {
            s.mouse = lparam_point(lparam);
            on_move(s);
            let _ = draw(s);
        }
        WM_LBUTTONDOWN => {
            s.mouse = lparam_point(lparam);
            on_down(s);
        }
        WM_LBUTTONUP => {
            s.mouse = lparam_point(lparam);
            on_up(s);
            if s.done {
                request_close(s);
            } else {
                let _ = draw(s);
            }
        }
        WM_RBUTTONUP => {
            request_close(s);
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                request_close(s);
            } else if vk == VK_RETURN.0 {
                confirm_current(s);
                if s.done {
                    request_close(s);
                }
            }
        }
        WM_OVERLAY_DONE => {
            s.done = true;
        }
        WM_TIMER => {
            let _ = draw(s);
        }
        WM_PAINT => {
            let _ = draw(s);
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

fn lparam_point(lp: LPARAM) -> Point {
    let x = (lp.0 as i32) & 0xFFFF;
    let y = ((lp.0 as i32) >> 16) & 0xFFFF;
    let x = x as i16 as i32;
    let y = y as i16 as i32;
    Point {
        x: x as f64,
        y: y as f64,
    }
}

fn on_down(s: &mut OverlayState) {
    s.down = true;
    s.down_pt = s.mouse;
    if s.phase == Phase::Active && s.has_sel {
        s.hit = hit_test(s.selection, s.mouse, 8.0);
        s.sel_at_press = s.selection;
    } else {
        s.hit = SelectionHitKind::None;
    }
}

fn on_move(s: &mut OverlayState) {
    if s.down && s.phase == Phase::Selecting {
        let dx = (s.mouse.x - s.down_pt.x).abs();
        let dy = (s.mouse.y - s.down_pt.y).abs();
        if dx > MIN_RECT || dy > MIN_RECT {
            s.selection = Rect::normalized(s.down_pt.x, s.down_pt.y, s.mouse.x, s.mouse.y);
            s.from_drag = true;
            s.has_sel = true;
        }
    } else if s.down && s.phase == Phase::Active {
        let bounds = Rect::new(0.0, 0.0, s.width as f64, s.height as f64);
        if s.hit == SelectionHitKind::Inside {
            s.selection = move_rect(
                s.sel_at_press,
                s.mouse.x - s.down_pt.x,
                s.mouse.y - s.down_pt.y,
                bounds,
            );
        } else if s.hit != SelectionHitKind::None {
            s.selection = resize_rect(s.sel_at_press, s.hit, s.mouse, bounds, MIN_RECT);
        }
    } else if s.phase == Phase::Selecting {
        s.hover = hit_window(&s.windows, s.mouse);
    }
}

fn on_up(s: &mut OverlayState) {
    if !s.down {
        return;
    }
    s.down = false;
    if s.phase == Phase::Selecting {
        let dx = (s.mouse.x - s.down_pt.x).abs();
        let dy = (s.mouse.y - s.down_pt.y).abs();
        if dx <= MIN_RECT && dy <= MIN_RECT {
            if let Some(h) = s.hover {
                s.selection = h;
                s.from_drag = false;
                s.has_sel = true;
                confirm_current(s);
            }
        } else if s.has_sel && s.selection.width > MIN_RECT && s.selection.height > MIN_RECT {
            s.phase = Phase::Active;
        }
    }
}

fn confirm_current(s: &mut OverlayState) {
    let rect = if s.has_sel {
        s.selection
    } else if let Some(h) = s.hover {
        s.from_drag = false;
        h
    } else {
        return;
    };
    if rect.width < 2.0 || rect.height < 2.0 {
        return;
    }
    let (x, y, w, h) = physical_source_rect(
        rect,
        s.from_drag,
        s.width as f64,
        s.height as f64,
        s.width as i32,
        s.height as i32,
    );
    if w < 2 || h < 2 {
        return;
    }
    s.result = OverlayResult {
        confirmed: true,
        x: x as u32,
        y: y as u32,
        w: w as u32,
        h: h as u32,
    };
    request_close(s);
}

unsafe fn draw(s: &mut OverlayState) -> Result<()> {
    let buf: ID3D11Texture2D = s.swap.GetBuffer(0)?;
    let surface: IDXGISurface = buf.cast()?;
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        colorContext: Default::default(),
    };
    let target: ID2D1Bitmap1 = s.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
    s.dc.SetTarget(&target);
    s.dc.BeginDraw();
    s.dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);

    let full = D2D_RECT_F {
        left: 0.0,
        top: 0.0,
        right: s.width as f32,
        bottom: s.height as f32,
    };
    s.dc.DrawBitmap(
        &s.freeze,
        Some(&full),
        1.0,
        D2D1_INTERPOLATION_MODE_LINEAR,
        None,
        None,
    );

    let dim = solid(&s.dc, 0.0, 0.0, 0.0, 0.2)?;
    s.dc.FillRectangle(&full, &dim);

    let shown = current_rect(s);
    if let Some(r) = shown {
        let dest = d2d_rect(r);
        s.dc.DrawBitmap(
            &s.freeze,
            Some(&dest),
            1.0,
            D2D1_INTERPOLATION_MODE_LINEAR,
            Some(&dest),
            None,
        );
        let white = solid(&s.dc, 1.0, 1.0, 1.0, 1.0)?;
        let black = solid(&s.dc, 0.0, 0.0, 0.0, 1.0)?;
        s.dc.DrawRectangle(&dest, &black, 1.0, None);
        let dash_offset = marching_ants_dash_offset(s.start.elapsed().as_secs_f32());
        let ants = marching_ants_stroke(&s.factory, dash_offset)?;
        s.dc.DrawRectangle(&dest, &white, 1.0, Some(&ants));
        if s.phase == Phase::Active {
            draw_handles(&s.dc, r)?;
        }
        let info = format!(
            "X: {:.0} Y: {:.0} W: {:.0} H: {:.0}",
            r.x, r.y, r.width, r.height
        );
        draw_label(&s.dc, &s.dwrite, &s.text_format, &info, r.x as f32 + 4.0, r.y as f32 + 4.0)?;
    }

    if s.phase == Phase::Selecting {
        draw_magnifier(s)?;
        let info = format!("X: {:.0} Y: {:.0}", s.mouse.x, s.mouse.y);
        draw_label(
            &s.dc,
            &s.dwrite,
            &s.text_format,
            &info,
            s.mouse.x as f32 + 12.0,
            s.mouse.y as f32 + 12.0,
        )?;
    }

    s.dc.EndDraw(None, None)?;
    s.dc.SetTarget(None);
    s.swap.Present(1, DXGI_PRESENT(0)).ok()?;
    let _ = COLORREF::default();
    let _ = PeekMessageW;
    Ok(())
}

fn current_rect(s: &OverlayState) -> Option<Rect> {
    if s.phase == Phase::Active && s.has_sel {
        return Some(s.selection);
    }
    if s.down && s.has_sel && s.selection.width > MIN_RECT {
        return Some(s.selection);
    }
    s.hover
}

fn marching_ants_stroke(factory: &ID2D1Factory1, dash_offset: f32) -> Result<ID2D1StrokeStyle> {
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        dashStyle: D2D1_DASH_STYLE_CUSTOM,
        dashOffset: dash_offset,
        miterLimit: 10.0,
        ..Default::default()
    };
    let base: ID2D1Factory = factory.cast()?;
    unsafe { base.CreateStrokeStyle(&props, Some(&ANTS_DASH)).map_err(Into::into) }
}

fn d2d_rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x as f32,
        top: r.y as f32,
        right: r.right() as f32,
        bottom: r.bottom() as f32,
    }
}

fn solid(dc: &ID2D1DeviceContext, r: f32, g: f32, b: f32, a: f32) -> Result<ID2D1SolidColorBrush> {
    unsafe {
        dc.CreateSolidColorBrush(
            &D2D1_COLOR_F { r, g, b, a },
            None,
        )
        .map_err(Into::into)
    }
}

fn draw_handles(dc: &ID2D1DeviceContext, r: Rect) -> Result<()> {
    let brush = solid(dc, 1.0, 1.0, 1.0, 1.0)?;
    let pts = [
        (r.x, r.y),
        (r.right(), r.y),
        (r.x, r.bottom()),
        (r.right(), r.bottom()),
        (r.x + r.width / 2.0, r.y),
        (r.x + r.width / 2.0, r.bottom()),
        (r.x, r.y + r.height / 2.0),
        (r.right(), r.y + r.height / 2.0),
    ];
    for (x, y) in pts {
        let rc = D2D_RECT_F {
            left: x as f32 - 3.0,
            top: y as f32 - 3.0,
            right: x as f32 + 3.0,
            bottom: y as f32 + 3.0,
        };
        unsafe { dc.FillRectangle(&rc, &brush) };
    }
    Ok(())
}

fn draw_magnifier(s: &OverlayState) -> Result<()> {
    let mag = MAG_COUNT * MAG_PIXEL;
    let mut dest_x = s.mouse.x as f32 + 10.0;
    let mut dest_y = s.mouse.y as f32 + 10.0;
    if dest_x + mag > s.width as f32 {
        dest_x = s.mouse.x as f32 - 10.0 - mag;
    }
    if dest_y + mag > s.height as f32 {
        dest_y = s.mouse.y as f32 - 10.0 - mag;
    }
    dest_x = dest_x.max(0.0);
    dest_y = dest_y.max(0.0);
    let half = (MAG_COUNT / 2.0).floor();
    let mut src_x = (s.mouse.x as f32).floor() - half;
    let mut src_y = (s.mouse.y as f32).floor() - half;
    src_x = src_x.clamp(0.0, s.width as f32 - MAG_COUNT);
    src_y = src_y.clamp(0.0, s.height as f32 - MAG_COUNT);
    let dest = D2D_RECT_F {
        left: dest_x,
        top: dest_y,
        right: dest_x + mag,
        bottom: dest_y + mag,
    };
    let src = D2D_RECT_F {
        left: src_x,
        top: src_y,
        right: src_x + MAG_COUNT,
        bottom: src_y + MAG_COUNT,
    };
    unsafe {
        s.dc.DrawBitmap(
            &s.freeze,
            Some(&dest),
            1.0,
            D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            Some(&src),
            None,
        );
        let white = solid(&s.dc, 1.0, 1.0, 1.0, 1.0)?;
        s.dc.DrawRectangle(&dest, &white, 1.0, None);
    }
    Ok(())
}

fn draw_label(
    dc: &ID2D1DeviceContext,
    dwrite: &IDWriteFactory,
    fmt: &IDWriteTextFormat,
    text: &str,
    x: f32,
    y: f32,
) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let layout = dwrite.CreateTextLayout(wide.as_slice(), fmt, 400.0, 24.0)?;
        let mut metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let bg = D2D_RECT_F {
            left: x - 3.0,
            top: y - 2.0,
            right: x + metrics.width + 6.0,
            bottom: y + metrics.height + 4.0,
        };
        let brush = solid(dc, 0.0, 0.0, 0.0, 0.8)?;
        dc.FillRectangle(&bg, &brush);
        let white = solid(dc, 1.0, 1.0, 1.0, 1.0)?;
        dc.DrawTextLayout(
            windows_numerics::Vector2 { X: x, Y: y },
            &layout,
            &white,
            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }
    Ok(())
}

unsafe fn create_d2d(
    hwnd: HWND,
    cap: &CapturedDesktop,
) -> Result<(
    IDXGISwapChain1,
    ID2D1DeviceContext,
    ID2D1Bitmap1,
    IDWriteFactory,
    IDWriteTextFormat,
    ID2D1Factory1,
)> {
    let factory: ID2D1Factory1 =
        D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let dxgi_dev: IDXGIDevice = cap.gpu.device.cast()?;
    let d2d_dev: ID2D1Device = factory.CreateDevice(&dxgi_dev)?;
    let dc: ID2D1DeviceContext = d2d_dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
    let dxgi_factory: IDXGIFactory2 = {
        let dev: windows::Win32::Graphics::Dxgi::IDXGIDevice = cap.gpu.device.cast()?;
        let adapter = dev.GetAdapter()?;
        adapter.GetParent()?
    };
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: cap.width,
        Height: cap.height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    let swap = dxgi_factory.CreateSwapChainForHwnd(&cap.gpu.device, hwnd, &desc, None, None)?;

    let surf: IDXGISurface = cap.sdr_display.cast()?;
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: Default::default(),
    };
    let freeze: ID2D1Bitmap1 = dc.CreateBitmapFromDxgiSurface(&surf, Some(&props))?;

    let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
    let text_format = dwrite.CreateTextFormat(
        PCWSTR(w("Consolas\0").as_ptr()),
        None,
        DWRITE_FONT_WEIGHT_REGULAR,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        13.0,
        PCWSTR(w("en-us\0").as_ptr()),
    )?;
    let _ = D2D_SIZE_U::default();
    let _ = SetLayeredWindowAttributes;
    let _ = LWA_ALPHA;
    let _ = WS_EX_LAYERED;
    Ok((swap, dc, freeze, dwrite, text_format, factory))
}

fn register_class() -> Result<Vec<u16>> {
    let name = w("ZshotOverlayClass\0");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: unsafe { GetModuleHandleW(None)?.into() },
        lpszClassName: PCWSTR(name.as_ptr()),
        hCursor: unsafe { LoadCursorW(None, IDC_CROSS).unwrap_or_default() },
        ..Default::default()
    };
    unsafe {
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            // already registered
        }
    }
    Ok(name)
}

fn detect_windows(self_hwnd: HWND, ox: i32, oy: i32) -> Vec<Rect> {
    let mut ctx = WinCtx {
        self_hwnd,
        ox,
        oy,
        rects: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(enum_win), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.rects
}

unsafe extern "system" fn enum_win(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let ctx = &mut *(lparam.0 as *mut WinCtx);
    if hwnd == ctx.self_hwnd {
        return windows::core::BOOL(1);
    }
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return windows::core::BOOL(1);
    }
    let mut cloaked: i32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut _ as *mut _,
        4,
    );
    if cloaked != 0 {
        return windows::core::BOOL(1);
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW_STYLE.0 != 0 && ex & WS_EX_NOACTIVATE.0 != 0 {
        return windows::core::BOOL(1);
    }
    let mut wr = RECT::default();
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut wr as *mut _ as *mut _,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_err()
        && GetWindowRect(hwnd, &mut wr).is_err()
    {
        return windows::core::BOOL(1);
    }
    let w = wr.right - wr.left;
    let h = wr.bottom - wr.top;
    if w <= 5 || h <= 5 {
        return windows::core::BOOL(1);
    }
    ctx.rects.push(Rect::new(
        (wr.left - ctx.ox) as f64,
        (wr.top - ctx.oy) as f64,
        w as f64,
        h as f64,
    ));
    let _ = GetClientRect;
    let _ = ClientToScreen;
    windows::core::BOOL(1)
}

struct WinCtx {
    self_hwnd: HWND,
    ox: i32,
    oy: i32,
    rects: Vec<Rect>,
}

fn hit_window(windows: &[Rect], p: Point) -> Option<Rect> {
    windows.iter().copied().find(|r| r.contains(p))
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ants_offset_matches_csharp_rate() {
        assert!((marching_ants_dash_offset(1.0) - ANTS_SPEED).abs() < 1e-5);
        assert!((marching_ants_dash_offset(0.0) - 0.0).abs() < 1e-5);
        assert!(marching_ants_dash_offset(2.0) < marching_ants_dash_offset(1.0));
    }

    #[test]
    fn ants_pattern_is_five_five() {
        assert_eq!(ANTS_DASH, [5.0, 5.0]);
    }
}

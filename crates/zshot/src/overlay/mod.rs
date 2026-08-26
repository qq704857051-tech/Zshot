//! GPUI freeze overlay: dim, marching ants, window snap, 15×15 magnifier.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, black, canvas, div, fill, hsla, outline, point, prelude::*, px, rgb, size, white, App,
    BorderStyle, Bounds, Context, CursorStyle, Entity, FocusHandle, Focusable, InteractiveElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels,
    Point as GpuiPoint, Timer, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions,
};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible,
    PostMessageW, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
    GWL_EXSTYLE, HWND_TOPMOST, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, WDA_EXCLUDEFROMCAPTURE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::platform::WM_APP_READY_SENTINEL;
use crate::capture::CapturedDesktop;
use crate::error::Result;
use crate::geom::{
    hit_test, move_rect, physical_source_rect, resize_rect, Point, Rect, SelectionHitKind,
};
use crate::gpui_bridge::present_freeze_from_texture;
use crate::perf::PerfSession;

const MAG_COUNT: u32 = 15;
const MAG_PIXEL: f32 = 10.0;
const MIN_RECT: f64 = 5.0;

/// C# RegionCaptureWindow: dash {5,5}, offset = elapsed * -15.
pub const ANTS_DASH: [f32; 2] = [5.0, 5.0];
pub const ANTS_SPEED: f32 = -15.0;

pub fn marching_ants_dash_offset(elapsed_secs: f32) -> f32 {
    elapsed_secs * ANTS_SPEED
}

pub fn bench_auto_hide_enabled() -> bool {
    std::env::var_os("ZSHOT_BENCH").is_some()
}

actions!(overlay, [ConfirmSelection, CancelOverlay]);

thread_local! {
    static OVERLAY_WINDOW: RefCell<Option<WindowHandle<OverlayView>>> = const { RefCell::new(None) };
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
}

pub struct OverlayResult {
    pub confirmed: bool,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Region overlay root view. Freeze is DComp visual 0; this paints UI visual 1.
pub struct OverlayView {
    pub selection: Rect,
    pub hover: Option<Rect>,
    pub mouse: Point,
    pub freeze_size: (u32, u32),
    focus_handle: FocusHandle,
    cap: Option<CapturedDesktop>,
    perf: Rc<RefCell<Option<PerfSession>>>,
    freeze_swap: Option<IDXGISwapChain1>,
    origin_x: i32,
    origin_y: i32,
    phase: Phase,
    down: bool,
    down_pt: Point,
    from_drag: bool,
    has_sel: bool,
    windows: Vec<Rect>,
    hit: SelectionHitKind,
    sel_at_press: Rect,
    start: Instant,
    visible: bool,
    mag: Option<(Vec<u8>, u32, u32, u32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Selecting,
    Active,
}

pub fn show_overlay(
    cx: &mut App,
    cap: CapturedDesktop,
    perf: &mut PerfSession,
) -> Entity<OverlayView> {
    bind_overlay_keys(cx);
    let session = std::mem::replace(perf, PerfSession::start());
    match show_overlay_inner(cx, cap, session) {
        Ok(entity) => entity,
        Err(e) => {
            eprintln!("overlay: {e}");
            crate::app::complete_region_shot(
                OverlayResult {
                    confirmed: false,
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                },
                None,
                None,
                cx,
            );
            cx.new(OverlayView::placeholder)
        }
    }
}

fn bind_overlay_keys(cx: &mut App) {
    if KEYS_BOUND.get() {
        return;
    }
    cx.bind_keys(vec![
        KeyBinding::new("enter", ConfirmSelection, Some("Overlay")),
        KeyBinding::new("escape", CancelOverlay, Some("Overlay")),
    ]);
    KEYS_BOUND.set(true);
}

fn show_overlay_inner(
    cx: &mut App,
    cap: CapturedDesktop,
    perf: PerfSession,
) -> Result<Entity<OverlayView>> {
    let swap = match cx.with_gpu_context(|device, context| {
        present_freeze_from_texture(device, context, &cap.sdr_display, cap.width, cap.height)
    }) {
        Some(Ok(s)) => s,
        Some(Err(e)) => return Err(e),
        None => return Err(crate::error::Error::new("GPU context unavailable")),
    };

    if let Some(handle) = OVERLAY_WINDOW.with(|slot| *slot.borrow()) {
        if let Ok(entity) = handle.entity(cx) {
            let _ = handle.update(cx, |view, window, cx| {
                view.reset_from_capture(cap, perf, swap, window);
                view.attach_and_show(window, cx);
            });
            return Ok(entity);
        }
    }

    let bounds = Bounds {
        origin: point(px(cap.origin_x as f32), px(cap.origin_y as f32)),
        size: size(px(cap.width as f32), px(cap.height as f32)),
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                show: false,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                topmost: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| OverlayView::from_capture(cx, cap, perf, swap, window)),
        )
        .map_err(|e| crate::error::Error::new(e.to_string()))?;

    OVERLAY_WINDOW.with(|slot| {
        *slot.borrow_mut() = Some(handle);
    });

    let entity = handle
        .entity(cx)
        .map_err(|e| crate::error::Error::new(e.to_string()))?;
    let _ = handle.update(cx, |view, window, cx| {
        view.attach_and_show(window, cx);
    });
    Ok(entity)
}

impl OverlayView {
    fn placeholder(cx: &mut Context<Self>) -> Self {
        Self {
            selection: Rect::new(0.0, 0.0, 0.0, 0.0),
            hover: None,
            mouse: Point { x: 0.0, y: 0.0 },
            freeze_size: (1, 1),
            focus_handle: cx.focus_handle(),
            cap: None,
            perf: Rc::new(RefCell::new(None)),
            freeze_swap: None,
            origin_x: 0,
            origin_y: 0,
            phase: Phase::Selecting,
            down: false,
            down_pt: Point { x: 0.0, y: 0.0 },
            from_drag: false,
            has_sel: false,
            windows: Vec::new(),
            hit: SelectionHitKind::None,
            sel_at_press: Rect::new(0.0, 0.0, 0.0, 0.0),
            start: Instant::now(),
            visible: false,
            mag: None,
        }
    }

    fn from_capture(
        cx: &mut Context<Self>,
        cap: CapturedDesktop,
        perf: PerfSession,
        swap: IDXGISwapChain1,
        window: &mut Window,
    ) -> Self {
        let mut view = Self {
            selection: Rect::new(0.0, 0.0, 0.0, 0.0),
            hover: None,
            mouse: Point { x: 0.0, y: 0.0 },
            freeze_size: (cap.width, cap.height),
            focus_handle: cx.focus_handle(),
            cap: Some(cap),
            perf: Rc::new(RefCell::new(Some(perf))),
            freeze_swap: Some(swap),
            origin_x: 0,
            origin_y: 0,
            phase: Phase::Selecting,
            down: false,
            down_pt: Point { x: 0.0, y: 0.0 },
            from_drag: false,
            has_sel: false,
            windows: Vec::new(),
            hit: SelectionHitKind::None,
            sel_at_press: Rect::new(0.0, 0.0, 0.0, 0.0),
            start: Instant::now(),
            visible: true,
            mag: None,
        };
        view.sync_from_cap(window);
        view
    }

    fn reset_from_capture(
        &mut self,
        cap: CapturedDesktop,
        perf: PerfSession,
        swap: IDXGISwapChain1,
        window: &mut Window,
    ) {
        self.selection = Rect::new(0.0, 0.0, 0.0, 0.0);
        self.hover = None;
        self.mouse = Point { x: 0.0, y: 0.0 };
        self.freeze_size = (cap.width, cap.height);
        self.cap = Some(cap);
        self.perf = Rc::new(RefCell::new(Some(perf)));
        self.freeze_swap = Some(swap);
        self.phase = Phase::Selecting;
        self.down = false;
        self.from_drag = false;
        self.has_sel = false;
        self.hit = SelectionHitKind::None;
        self.start = Instant::now();
        self.visible = true;
        self.mag = None;
        self.sync_from_cap(window);
    }

    fn sync_from_cap(&mut self, window: &Window) {
        let (ox, oy, w, h) = match &self.cap {
            Some(c) => (c.origin_x, c.origin_y, c.width, c.height),
            None => return,
        };
        self.origin_x = ox;
        self.origin_y = oy;
        self.freeze_size = (w, h);
        let hwnd = window.win32_hwnd();
        self.windows = detect_windows(hwnd, ox, oy);
        let mut mouse = Point { x: 0.0, y: 0.0 };
        let mut pt = POINT::default();
        unsafe {
            if GetCursorPos(&mut pt).is_ok() {
                mouse.x = (pt.x - ox) as f64;
                mouse.y = (pt.y - oy) as f64;
            }
        }
        self.mouse = mouse;
        self.down_pt = mouse;
        self.hover = hit_window(&self.windows, mouse);
    }

    fn attach_and_show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(swap) = &self.freeze_swap {
            if let Err(e) = window.set_background_swap_chain(Some(swap)) {
                eprintln!("set_background_swap_chain: {e}");
            }
        }
        let hwnd = window.win32_hwnd();
        unsafe {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                self.origin_x,
                self.origin_y,
                self.freeze_size.0 as i32,
                self.freeze_size.1 as i32,
                SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        window.focus(&self.focus_handle);
        window.on_window_should_close(cx, |_window, cx| {
            if let Some(handle) = OVERLAY_WINDOW.with(|s| *s.borrow()) {
                let _ = handle.update(cx, |view, window, cx| {
                    view.cancel(window, cx);
                });
            }
            false
        });
        let perf = self.perf.clone();
        window.on_app_ready_sentinel({
            let perf = self.perf.clone();
            move || {
                if let Some(p) = perf.borrow_mut().as_mut() {
                    p.mark("T7");
                }
            }
        });
        window.on_next_frame(move |window, _cx| {
            let _ = window.wait_for_commit_completion();
            if let Some(p) = perf.borrow_mut().as_mut() {
                p.mark("T6");
            }
            let hwnd = window.win32_hwnd();
            unsafe {
                let _ = SetForegroundWindow(hwnd);
                let _ = PostMessageW(Some(hwnd), WM_APP_READY_SENTINEL, WPARAM(0), LPARAM(0));
            }
        });
        // Do not call request_animation_frame here — it requires a paint-time
        // current_view. focus() already refresh()ed; notify + vsync will draw.
        if bench_auto_hide_enabled() {
            self.schedule_bench_auto_hide(cx);
        }
        self.refresh_magnifier_gpu(cx);
        cx.notify();
    }

    fn schedule_bench_auto_hide(&self, cx: &mut Context<Self>) {
        let perf = self.perf.clone();
        cx.spawn(async move |_this, cx| {
            for _ in 0..5_000 {
                Timer::after(Duration::from_millis(1)).await;
                let ready = perf
                    .borrow()
                    .as_ref()
                    .map(|p| p.has_mark("T7"))
                    .unwrap_or(true);
                if ready {
                    break;
                }
            }
            let _ = cx.update(|cx| {
                if let Some(handle) = OVERLAY_WINDOW.with(|s| *s.borrow()) {
                    let _ = handle.update(cx, |view, window, cx| {
                        view.cancel(window, cx);
                    });
                }
            });
        })
        .detach();
    }

    fn hide_window(window: &Window) {
        unsafe {
            let _ = ShowWindow(window.win32_hwnd(), SW_HIDE);
        }
    }

    fn pointer_to_capture(pos: GpuiPoint<Pixels>, scale: f32) -> Point {
        Point {
            x: (f32::from(pos.x) * scale) as f64,
            y: (f32::from(pos.y) * scale) as f64,
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible {
            return;
        }
        if event.button == MouseButton::Right {
            self.cancel(window, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.mouse = Self::pointer_to_capture(event.position, window.scale_factor());
        self.down = true;
        self.down_pt = self.mouse;
        if self.phase == Phase::Active && self.has_sel {
            self.hit = hit_test(self.selection, self.mouse, 8.0);
            self.sel_at_press = self.selection;
        } else {
            self.hit = SelectionHitKind::None;
        }
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible {
            return;
        }
        self.mouse = Self::pointer_to_capture(event.position, window.scale_factor());
        self.on_move();
        if self.phase == Phase::Selecting {
            self.refresh_magnifier_gpu(cx);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        if event.button == MouseButton::Right {
            self.cancel(window, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.mouse = Self::pointer_to_capture(event.position, window.scale_factor());
        let confirm = self.on_up();
        if confirm {
            self.confirm(window, cx);
        } else {
            cx.notify();
        }
    }

    fn on_move(&mut self) {
        if self.down && self.phase == Phase::Selecting {
            let dx = (self.mouse.x - self.down_pt.x).abs();
            let dy = (self.mouse.y - self.down_pt.y).abs();
            if dx > MIN_RECT || dy > MIN_RECT {
                self.selection =
                    Rect::normalized(self.down_pt.x, self.down_pt.y, self.mouse.x, self.mouse.y);
                self.from_drag = true;
                self.has_sel = true;
            }
        } else if self.down && self.phase == Phase::Active {
            let bounds = Rect::new(
                0.0,
                0.0,
                self.freeze_size.0 as f64,
                self.freeze_size.1 as f64,
            );
            if self.hit == SelectionHitKind::Inside {
                self.selection = move_rect(
                    self.sel_at_press,
                    self.mouse.x - self.down_pt.x,
                    self.mouse.y - self.down_pt.y,
                    bounds,
                );
            } else if self.hit != SelectionHitKind::None {
                self.selection =
                    resize_rect(self.sel_at_press, self.hit, self.mouse, bounds, MIN_RECT);
            }
        } else if self.phase == Phase::Selecting {
            self.hover = hit_window(&self.windows, self.mouse);
        }
    }

    fn on_up(&mut self) -> bool {
        if !self.down {
            return false;
        }
        self.down = false;
        if self.phase == Phase::Selecting {
            let dx = (self.mouse.x - self.down_pt.x).abs();
            let dy = (self.mouse.y - self.down_pt.y).abs();
            if dx <= MIN_RECT && dy <= MIN_RECT {
                if let Some(h) = self.hover {
                    self.selection = h;
                    self.from_drag = false;
                    self.has_sel = true;
                    return true;
                }
            } else if self.has_sel
                && self.selection.width > MIN_RECT
                && self.selection.height > MIN_RECT
            {
                self.phase = Phase::Active;
            }
        }
        false
    }

    fn current_rect(&self) -> Option<Rect> {
        if self.phase == Phase::Active && self.has_sel {
            return Some(self.selection);
        }
        if self.down && self.has_sel && self.selection.width > MIN_RECT {
            return Some(self.selection);
        }
        self.hover
    }

    fn result_from_current(&self) -> Option<OverlayResult> {
        let rect = if self.has_sel {
            self.selection
        } else {
            self.hover?
        };
        if rect.width < 2.0 || rect.height < 2.0 {
            return None;
        }
        let from_drag = if self.has_sel { self.from_drag } else { false };
        let (x, y, w, h) = physical_source_rect(
            rect,
            from_drag,
            self.freeze_size.0 as f64,
            self.freeze_size.1 as f64,
            self.freeze_size.0 as i32,
            self.freeze_size.1 as i32,
        );
        if w < 2 || h < 2 {
            return None;
        }
        Some(OverlayResult {
            confirmed: true,
            x: x as u32,
            y: y as u32,
            w: w as u32,
            h: h as u32,
        })
    }

    fn confirm_action(
        &mut self,
        _: &ConfirmSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm(window, cx);
    }

    fn cancel_action(&mut self, _: &CancelOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel(window, cx);
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let Some(result) = self.result_from_current() else {
            return;
        };
        self.finish(result, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        self.finish(
            OverlayResult {
                confirmed: false,
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            window,
            cx,
        );
    }

    fn finish(&mut self, result: OverlayResult, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = false;
        Self::hide_window(window);
        let cap = self.cap.take();
        let perf = self.perf.borrow_mut().take();
        crate::app::complete_region_shot(result, cap, perf, cx);
        cx.notify();
    }

    fn refresh_magnifier_gpu(&mut self, cx: &mut App) {
        let Some(cap) = self.cap.as_ref() else {
            self.mag = None;
            return;
        };
        let half = (MAG_COUNT / 2) as i32;
        let max_x = cap.width.saturating_sub(MAG_COUNT) as i32;
        let max_y = cap.height.saturating_sub(MAG_COUNT) as i32;
        let x = (self.mouse.x.floor() as i32 - half).clamp(0, max_x) as u32;
        let y = (self.mouse.y.floor() as i32 - half).clamp(0, max_y) as u32;
        self.mag = cx
            .with_gpu_context(|_device, ctx| {
                crate::capture::crop_sdr_bgra(cap, ctx, x, y, MAG_COUNT, MAG_COUNT)
            })
            .and_then(|r| r.ok());
    }
}

impl Focusable for OverlayView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.visible {
            window.request_animation_frame();
        }
        let scale = window.scale_factor().max(0.01);
        let shown = self.current_rect();
        let mouse = self.mouse;
        let mag = self.mag.clone();
        let phase = self.phase;
        let start = self.start;
        let has_sel = self.has_sel;

        div()
            .id("overlay-root")
            .size_full()
            .key_context("Overlay")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::Crosshair)
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_action(cx.listener(Self::confirm_action))
            .on_action(cx.listener(Self::cancel_action))
            .child(
                canvas(
                    move |_, _, _| (),
                    move |bounds, _, window, _cx| {
                        paint_overlay(window, bounds, OverlayPaint {
                            shown,
                            mouse,
                            mag,
                            phase,
                            start,
                            has_sel,
                            scale,
                        });
                    },
                )
                .size_full(),
            )
            .child(coord_label(shown, mouse, phase, scale))
    }
}

fn coord_label(
    shown: Option<Rect>,
    mouse: Point,
    phase: Phase,
    scale: f32,
) -> impl IntoElement {
    let (text, x, y) = if let Some(r) = shown {
        (
            format!(
                "X: {:.0} Y: {:.0} W: {:.0} H: {:.0}",
                r.x, r.y, r.width, r.height
            ),
            r.x as f32 / scale + 4.0,
            r.y as f32 / scale + 4.0,
        )
    } else if phase == Phase::Selecting {
        (
            format!("X: {:.0} Y: {:.0}", mouse.x, mouse.y),
            mouse.x as f32 / scale + 12.0,
            mouse.y as f32 / scale + 12.0,
        )
    } else {
        return div();
    };
    div()
        .absolute()
        .left(px(x.max(0.0)))
        .top(px(y.max(0.0)))
        .bg(hsla(0., 0., 0., 0.8))
        .text_color(white())
        .text_xs()
        .px(px(4.))
        .py(px(2.))
        .child(text)
}

struct OverlayPaint {
    shown: Option<Rect>,
    mouse: Point,
    mag: Option<(Vec<u8>, u32, u32, u32)>,
    phase: Phase,
    start: Instant,
    has_sel: bool,
    scale: f32,
}

fn paint_overlay(window: &mut Window, bounds: Bounds<Pixels>, paint: OverlayPaint) {
    let OverlayPaint {
        shown,
        mouse,
        mag,
        phase,
        start,
        has_sel,
        scale,
    } = paint;
    let hole = shown.map(|r| rect_to_bounds(r, scale));
    for dim in dim_around(bounds, hole) {
        window.paint_quad(fill(dim, hsla(0., 0., 0., 0.2)));
    }
    if let Some(r) = shown {
        let dest = rect_to_bounds(r, scale);
        window.paint_quad(outline(dest, black(), BorderStyle::Solid));
        paint_ants(window, dest, start.elapsed().as_secs_f32());
        if phase == Phase::Active && has_sel {
            paint_handles(window, dest, scale);
        }
    }
    if phase == Phase::Selecting {
        if let Some(pixels) = mag {
            paint_magnifier(window, bounds, mouse, pixels, scale);
        }
    }
}

fn rect_to_bounds(r: Rect, scale: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(r.x as f32 / scale), px(r.y as f32 / scale)),
        size: size(px(r.width as f32 / scale), px(r.height as f32 / scale)),
    }
}

fn dim_around(full: Bounds<Pixels>, hole: Option<Bounds<Pixels>>) -> Vec<Bounds<Pixels>> {
    let Some(hole) = hole else {
        return vec![full];
    };
    let mut out = Vec::new();
    if hole.origin.y > full.origin.y {
        out.push(Bounds {
            origin: full.origin,
            size: size(full.size.width, hole.origin.y - full.origin.y),
        });
    }
    let hole_bottom = hole.origin.y + hole.size.height;
    let full_bottom = full.origin.y + full.size.height;
    if hole_bottom < full_bottom {
        out.push(Bounds {
            origin: point(full.origin.x, hole_bottom),
            size: size(full.size.width, full_bottom - hole_bottom),
        });
    }
    if hole.origin.x > full.origin.x {
        out.push(Bounds {
            origin: point(full.origin.x, hole.origin.y),
            size: size(hole.origin.x - full.origin.x, hole.size.height),
        });
    }
    let hole_right = hole.origin.x + hole.size.width;
    let full_right = full.origin.x + full.size.width;
    if hole_right < full_right {
        out.push(Bounds {
            origin: point(hole_right, hole.origin.y),
            size: size(full_right - hole_right, hole.size.height),
        });
    }
    out
}

fn ants_dash_array(elapsed: f32) -> Vec<Pixels> {
    let offset = marching_ants_dash_offset(elapsed);
    let period = ANTS_DASH[0] + ANTS_DASH[1];
    let o = ((-offset) % period + period) % period;
    if o < 0.05 {
        vec![px(ANTS_DASH[0]), px(ANTS_DASH[1])]
    } else {
        vec![px(0.01), px(o), px(ANTS_DASH[0]), px(ANTS_DASH[1])]
    }
}

fn paint_ants(window: &mut Window, dest: Bounds<Pixels>, elapsed: f32) {
    let mut path = PathBuilder::stroke(px(1.)).dash_array(&ants_dash_array(elapsed));
    let origin = dest.origin;
    let tr = point(dest.origin.x + dest.size.width, dest.origin.y);
    let br = point(
        dest.origin.x + dest.size.width,
        dest.origin.y + dest.size.height,
    );
    let bl = point(dest.origin.x, dest.origin.y + dest.size.height);
    path.move_to(origin);
    path.line_to(tr);
    path.line_to(br);
    path.line_to(bl);
    path.close();
    if let Ok(p) = path.build() {
        window.paint_path(p, white());
    }
}

fn paint_handles(window: &mut Window, dest: Bounds<Pixels>, scale: f32) {
    let half = px(3.0 / scale);
    let pts = [
        dest.origin,
        point(dest.origin.x + dest.size.width, dest.origin.y),
        point(dest.origin.x, dest.origin.y + dest.size.height),
        point(
            dest.origin.x + dest.size.width,
            dest.origin.y + dest.size.height,
        ),
        point(dest.origin.x + dest.size.width / 2., dest.origin.y),
        point(
            dest.origin.x + dest.size.width / 2.,
            dest.origin.y + dest.size.height,
        ),
        point(dest.origin.x, dest.origin.y + dest.size.height / 2.),
        point(
            dest.origin.x + dest.size.width,
            dest.origin.y + dest.size.height / 2.,
        ),
    ];
    for p in pts {
        window.paint_quad(fill(
            Bounds {
                origin: point(p.x - half, p.y - half),
                size: size(half * 2., half * 2.),
            },
            white(),
        ));
    }
}

fn paint_magnifier(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    mouse: Point,
    pixels: (Vec<u8>, u32, u32, u32),
    scale: f32,
) {
    let (px_buf, mw, mh, stride) = pixels;
    let mag = MAG_COUNT as f32 * MAG_PIXEL / scale;
    let mut dest_x = mouse.x as f32 / scale + 10.0 / scale;
    let mut dest_y = mouse.y as f32 / scale + 10.0 / scale;
    let right = f32::from(bounds.origin.x) + f32::from(bounds.size.width);
    let bottom = f32::from(bounds.origin.y) + f32::from(bounds.size.height);
    if dest_x + mag > right {
        dest_x = mouse.x as f32 / scale - 10.0 / scale - mag;
    }
    if dest_y + mag > bottom {
        dest_y = mouse.y as f32 / scale - 10.0 / scale - mag;
    }
    dest_x = dest_x.max(0.0);
    dest_y = dest_y.max(0.0);
    let cell = MAG_PIXEL / scale;
    // ponytail: 15x15 = 225 quads per frame, batched by the renderer. Upgrade to a
    // single sampled texture only if the overlay frame budget actually shows it.
    for y in 0..mh {
        for x in 0..mw {
            let i = (y * stride + x * 4) as usize;
            if i + 2 >= px_buf.len() {
                continue;
            }
            let b = px_buf[i] as u32;
            let g = px_buf[i + 1] as u32;
            let r = px_buf[i + 2] as u32;
            let color = rgb((r << 16) | (g << 8) | b);
            let origin = point(px(dest_x + x as f32 * cell), px(dest_y + y as f32 * cell));
            window.paint_quad(fill(
                Bounds {
                    origin,
                    size: size(px(cell), px(cell)),
                },
                color,
            ));
        }
    }
    window.paint_quad(outline(
        Bounds {
            origin: point(px(dest_x), px(dest_y)),
            size: size(px(mag), px(mag)),
        },
        white(),
        BorderStyle::Solid,
    ));
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
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as *mut _, 4);
    if cloaked != 0 {
        return windows::core::BOOL(1);
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_NOACTIVATE.0 != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The dash phase must stay in [0, period) for every elapsed value, including
    /// large ones: `offset` is negative and grows without bound, so a single `%`
    /// would hand a negative leading gap to PathBuilder.
    #[test]
    fn ants_dash_phase_is_never_negative() {
        for ms in 0..2000 {
            let arr = ants_dash_array(ms as f32 / 100.0);
            for p in &arr {
                assert!(f32::from(*p) >= 0.0, "elapsed={ms} produced {arr:?}");
            }
        }
    }

    #[test]
    fn ants_dash_wraps_at_one_period() {
        let period = ANTS_DASH[0] + ANTS_DASH[1];
        // phase 0 => plain {5,5}, no leading gap.
        assert_eq!(ants_dash_array(0.0).len(), 2);
        // one full period later the pattern is back to phase 0.
        let one_period = period / ANTS_SPEED.abs();
        assert_eq!(ants_dash_array(one_period).len(), 2);
        // mid-period inserts a leading gap equal to the phase.
        let mid = ants_dash_array(0.2);
        assert_eq!(mid.len(), 4);
        assert!((f32::from(mid[1]) - 3.0).abs() < 1e-3, "{mid:?}");
    }
}

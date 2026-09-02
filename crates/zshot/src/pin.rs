//! Pin-to-screen: the annotated crop becomes a topmost, borderless window that
//! stays on the desktop after the overlay closes (PixPin 钉图). Basic version:
//! display, drag, wheel zoom, Esc / double-click to close. Any number of pin
//! windows can be alive at once; the registry below keeps their handles so the
//! windows are not dropped.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use gpui::{
    actions, div, hsla, img, prelude::*, px, size, white, App, Bounds, Context, CursorStyle,
    FocusHandle, Focusable, ImageSource, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollWheelEvent, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions,
};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetWindowRect, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOZORDER, SWP_SHOWWINDOW,
};

actions!(pin, [ClosePin]);

thread_local! {
    static PIN_WINDOWS: RefCell<Vec<WindowHandle<PinView>>> = const { RefCell::new(Vec::new()) };
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
}

const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 10.0;
const ZOOM_STEP: f32 = 1.1;

/// Opens one pin window. `pixels` is the flattened annotated crop (BGRA,
/// physical pixels); `(screen_x, screen_y)` is where the crop sat on the
/// virtual desktop, so the pin lands exactly over what the user captured.
pub fn show_pin(
    cx: &mut App,
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    stride: u32,
    screen_x: i32,
    screen_y: i32,
) {
    let buf = repack_opaque(&pixels, w, h, stride);
    let Some(rgba) = image::RgbaImage::from_raw(w, h, buf) else {
        crate::perf::log_app("pin: pixel buffer size mismatch");
        return;
    };
    let image = Arc::new(RenderImage::new(vec![image::Frame::new(rgba)]));

    bind_pin_keys(cx);
    // Bounds here are logical; attach_to_screen repositions in physical pixels
    // once the window exists, same pattern as the overlay.
    let bounds = Bounds {
        origin: GpuiPoint::new(px(screen_x as f32), px(screen_y as f32)),
        size: size(px(w as f32), px(h as f32)),
    };
    let handle = match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: true,
            show: true,
            kind: gpui::WindowKind::Normal,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            topmost: true,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| PinView::new(cx, image, w, h, window)),
    ) {
        Ok(handle) => handle,
        Err(e) => {
            crate::perf::log_app(&format!("pin window failed: {e}"));
            return;
        }
    };

    PIN_WINDOWS.with(|slot| slot.borrow_mut().push(handle));
    let _ = handle.update(cx, |view, window, cx| {
        view.attach_to_screen(window, screen_x, screen_y);
        window.focus(&view.focus_handle);
        window.on_window_should_close(cx, move |_, cx| {
            PIN_WINDOWS.with(|slot| {
                slot.borrow_mut().retain(|h| *h != handle);
            });
            cx.refresh_windows();
            true
        });
    });
}

fn bind_pin_keys(cx: &mut App) {
    if KEYS_BOUND.get() {
        return;
    }
    cx.bind_keys(vec![KeyBinding::new("escape", ClosePin, Some("Pin"))]);
    KEYS_BOUND.set(true);
}

pub struct PinView {
    image: Arc<RenderImage>,
    /// Physical pixel size of the flattened crop.
    pixel_size: (u32, u32),
    zoom: f32,
    /// Cursor position minus the window origin at drag start, screen coords.
    drag_offset: Option<(i32, i32)>,
    focus_handle: FocusHandle,
}

impl PinView {
    fn new(
        cx: &mut Context<Self>,
        image: Arc<RenderImage>,
        w: u32,
        h: u32,
        _window: &mut Window,
    ) -> Self {
        Self {
            image,
            pixel_size: (w.max(1), h.max(1)),
            zoom: 1.0,
            drag_offset: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Exact placement: GPUI's window_bounds are logical pixels, and the
    /// caption-free window still carries non-client insets, so set the outer
    /// rect whose client area matches the crop at its original screen position.
    fn attach_to_screen(&mut self, window: &mut Window, screen_x: i32, screen_y: i32) {
        let hwnd = window.win32_hwnd();
        let desired = RECT {
            left: screen_x,
            top: screen_y,
            right: screen_x + self.pixel_size.0 as i32,
            bottom: screen_y + self.pixel_size.1 as i32,
        };
        let outer = window_rect_for_client_area(hwnd, desired);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                outer.left,
                outer.top,
                outer.right - outer.left,
                outer.bottom - outer.top,
                SWP_SHOWWINDOW,
            );
        }
    }

    fn close(&mut self, _: &ClosePin, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        if event.click_count >= 2 {
            window.remove_window();
            return;
        }
        let hwnd = window.win32_hwnd();
        let mut cursor = POINT::default();
        let mut rect = RECT::default();
        unsafe {
            if GetCursorPos(&mut cursor).is_ok() && GetWindowRect(hwnd, &mut rect).is_ok() {
                self.drag_offset = Some((cursor.x - rect.left, cursor.y - rect.top));
            }
        }
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // Basic version: dragging only tracks while the cursor is over the
        // window (GPUI delivers no moves outside it). The offset keeps the
        // grab point stable, so this still feels right at normal speeds.
        if !event.dragging() {
            return;
        }
        let Some((dx, dy)) = self.drag_offset else {
            return;
        };
        let mut cursor = POINT::default();
        unsafe {
            if GetCursorPos(&mut cursor).is_err() {
                return;
            }
            let _ = SetWindowPos(
                window.win32_hwnd(),
                None,
                cursor.x - dx,
                cursor.y - dy,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
            );
        }
    }

    fn on_mouse_up(&mut self, event: &gpui::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.drag_offset.take().is_some() {
            cx.notify();
        }
    }

    /// Wheel zoom resizes the window with the image -- the frame hugs the
    /// pixels at every zoom level -- anchored on the cursor, so the image
    /// point under it stays put (the PixPin zoom feel).
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * 32.0,
        };
        if dy == 0.0 {
            return;
        }
        let factor = if dy > 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
        // A thin strip must not zoom into a zero-pixel window: keep the
        // smaller side at a grabbable 24 physical px.
        let min_dim = self.pixel_size.0.min(self.pixel_size.1) as f32;
        let floor = MIN_ZOOM.max(24.0 / min_dim);
        let new_zoom = (self.zoom * factor).clamp(floor, MAX_ZOOM);
        if new_zoom == self.zoom {
            return;
        }
        let old_zoom = self.zoom;
        self.zoom = new_zoom;

        let hwnd = window.win32_hwnd();
        let scale = window.scale_factor().max(0.01);
        let mut client_origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(hwnd, &mut client_origin);
        }
        // Cursor in screen pixels; event.position is logical, client-relative.
        let cursor_x = client_origin.x as f32 + f32::from(event.position.x) * scale;
        let cursor_y = client_origin.y as f32 + f32::from(event.position.y) * scale;
        // Image pixels (at 100%) between the client origin and the cursor.
        let rel_x = (cursor_x - client_origin.x as f32) / old_zoom;
        let rel_y = (cursor_y - client_origin.y as f32) / old_zoom;
        let new_cx = (cursor_x - rel_x * new_zoom).round() as i32;
        let new_cy = (cursor_y - rel_y * new_zoom).round() as i32;
        let new_w = (self.pixel_size.0 as f32 * new_zoom).round().max(1.0) as i32;
        let new_h = (self.pixel_size.1 as f32 * new_zoom).round().max(1.0) as i32;
        let desired = RECT {
            left: new_cx,
            top: new_cy,
            right: new_cx + new_w,
            bottom: new_cy + new_h,
        };
        let outer = window_rect_for_client_area(hwnd, desired);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                outer.left,
                outer.top,
                outer.right - outer.left,
                outer.bottom - outer.top,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        cx.notify();
    }
}

impl Focusable for PinView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PinView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = window.scale_factor().max(0.01);
        let w = self.pixel_size.0 as f32 / scale * self.zoom;
        let h = self.pixel_size.1 as f32 / scale * self.zoom;
        div()
            .id("pin-root")
            .size_full()
            .overflow_hidden()
            .key_context("Pin")
            .track_focus(&self.focus_handle)
            .occlude()
            .rounded_md()
            .border_1()
            .border_color(hsla(0., 0., 0., 0.45))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_action(cx.listener(Self::close))
            .child(
                img(ImageSource::Render(self.image.clone()))
                    .w(px(w))
                    .h(px(h)),
            )
            // A close affordance that does not cover small pins: under 48
            // logical px on either side, Esc / double-click remain the way out.
            .children((w >= 48.0 && h >= 48.0).then(|| {
                div()
                    .id("pin-close")
                    .absolute()
                    .top(px(6.))
                    .right(px(6.))
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(hsla(0., 0., 0., 0.55))
                    .text_color(white())
                    .text_size(px(12.))
                    .cursor(CursorStyle::PointingHand)
                    .hover(|s| s.bg(hsla(0.02, 0.7, 0.5, 0.9)))
                    // Swallow the press: it must not start a drag or read as
                    // a double-click close on the second hit.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|_, _, window, cx| {
                            cx.stop_propagation();
                            window.remove_window();
                        }),
                    )
                    .child("✕")
            }))
    }
}

/// Packs a possibly-padded BGRA buffer to a tight `width * 4` stride and
/// forces every pixel opaque -- the crop is opaque, but the source alpha can
/// be 0 from some desktop sources, which would render as blank in gpui.
fn repack_opaque(pixels: &[u8], w: u32, h: u32, stride: u32) -> Vec<u8> {
    let tight = (w * 4) as usize;
    let mut out = Vec::with_capacity(tight * h as usize);
    for row in 0..h as usize {
        let start = row * stride as usize;
        out.extend_from_slice(&pixels[start..start + tight]);
    }
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    out
}

/// Outer HWND rect whose client area equals `desired` (screen coordinates),
/// mirroring the overlay's helper. A caption-free window still reports small
/// non-client insets on some themes; without this the pinned image shifts off
/// the captured pixels.
fn window_rect_for_client_area(hwnd: HWND, desired: RECT) -> RECT {
    let mut window = RECT::default();
    let mut client = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        if GetWindowRect(hwnd, &mut window).is_err()
            || GetClientRect(hwnd, &mut client).is_err()
            || !ClientToScreen(hwnd, &mut origin).as_bool()
        {
            return desired;
        }
    }
    let left = origin.x - window.left;
    let top = origin.y - window.top;
    let right = window.right - (origin.x + client.right - client.left);
    let bottom = window.bottom - (origin.y + client.bottom - client.top);
    RECT {
        left: desired.left - left,
        top: desired.top - top,
        right: desired.right + right,
        bottom: desired.bottom + bottom,
    }
}

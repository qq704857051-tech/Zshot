//! GPUI freeze overlay: dim, marching ants, window snap, 15×15 magnifier.

mod assets;
pub(crate) mod composite;
mod editor;
mod theme;
mod toolbar;

pub use assets::ToolbarAssets;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, black, canvas, div, fill, hsla, outline, point, prelude::*, px, rgb, size, svg, white,
    App, BorderStyle, Bounds, BoxShadow, Context, CursorStyle, Entity, FocusHandle, Focusable,
    InteractiveElement, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    AnyView, ContentMask, MouseUpEvent, PathBuilder, Pixels, Point as GpuiPoint, SharedString,
    Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsIconic,
    IsWindowVisible, PostMessageW, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW,
    WDA_EXCLUDEFROMCAPTURE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::platform::WM_APP_READY_SENTINEL;
use crate::capture::{crop_sdr_bgra, enumerate_monitors, CapturedDesktop};
use crate::error::Result;
use crate::overlay::composite::{element_mask, CoverageMask};
use crate::overlay::editor::{Element, PressKind, Session, Shape, Tool};
use crate::overlay::toolbar::PHASE1_ITEMS;
use crate::geom::{
    hit_test, move_rect, physical_source_rect, resize_rect, Point, Rect, SelectionHitKind,
};
use crate::gpui_bridge::present_freeze_from_texture;
use crate::perf::PerfSession;

const MAG_COUNT: u32 = 15;
const MAG_PIXEL: f32 = 10.0;
const MIN_RECT: f64 = 5.0;

/// Complete / Enter: clipboard only. Save writes a file.
fn complete_export_flags() -> (bool, bool) {
    (false, true)
}

/// C# RegionCaptureWindow: dash {5,5}, offset = elapsed * -15.
pub const ANTS_DASH: [f32; 2] = [5.0, 5.0];
pub const ANTS_SPEED: f32 = -15.0;

pub fn marching_ants_dash_offset(elapsed_secs: f32) -> f32 {
    elapsed_secs * ANTS_SPEED
}

pub fn bench_auto_hide_enabled() -> bool {
    std::env::var_os("ZSHOT_BENCH").is_some()
}

actions!(overlay, [ConfirmSelection, CancelOverlay, UndoEdit, RedoEdit]);

thread_local! {
    static OVERLAY_WINDOW: RefCell<Option<WindowHandle<OverlayView>>> = const { RefCell::new(None) };
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct OverlayResult {
    pub confirmed: bool,
    pub write_file: bool,
    pub copy: bool,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub marks: Vec<Element>,
}

fn empty_result() -> OverlayResult {
    OverlayResult {
        confirmed: false,
        write_file: false,
        copy: false,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        marks: Vec::new(),
    }
}

/// Identity of a mosaic readback: which elements, where, at what block size, and
/// how many path points -- extending a stroke inside its own bounds moves nothing else.
type MosaicKey = Vec<(u64, Rect, i32, usize)>;

#[derive(Clone, Default)]
struct MosaicTiles {
    tiles: Vec<(Rect, u32)>,
    covered: Vec<u64>,
}

/// Cached coverage masks aligned 1:1 with the element list that produced them.
type MarkMasks = (Vec<Element>, Vec<Option<Rc<CoverageMask>>>);

struct TextDraft {
    pt: Point,
    buf: String,
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
    editor: Session,
    text_edit: Option<TextDraft>,
    /// Last mosaic readback, keyed by the elements that produced it. The overlay
    /// repaints every frame; without this each mosaic costs a GPU readback per frame.
    mosaic_cache: RefCell<Option<(MosaicKey, MosaicTiles)>>,
    /// Coverage masks for the annotation elements, keyed by the exact element list.
    /// The overlay repaints every frame for the marching ants; rasterizing the
    /// strokes again only happens when the annotations actually change.
    mask_cache: RefCell<Option<MarkMasks>>,
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
            crate::app::complete_region_shot(empty_result(), None, None, cx);
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
        KeyBinding::new("ctrl-z", UndoEdit, Some("Overlay")),
        KeyBinding::new("ctrl-y", RedoEdit, Some("Overlay")),
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
            editor: Session::new(),
            text_edit: None,
            mosaic_cache: RefCell::new(None),
            mask_cache: RefCell::new(None),
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
            editor: Session::new(),
            text_edit: None,
            mosaic_cache: RefCell::new(None),
            mask_cache: RefCell::new(None),
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
        self.editor = Session::new();
        self.text_edit = None;
        self.mask_cache = RefCell::new(None);
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
        let desired = RECT {
            left: self.origin_x,
            top: self.origin_y,
            right: self.origin_x + self.freeze_size.0 as i32,
            bottom: self.origin_y + self.freeze_size.1 as i32,
        };
        let outer = window_rect_for_client_area(hwnd, desired);
        unsafe {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                outer.left,
                outer.top,
                outer.right - outer.left,
                outer.bottom - outer.top,
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
            if self.text_edit.take().is_some() {
                cx.notify();
                return;
            }
            self.cancel(window, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let scale = window.scale_factor();
        self.mouse = Self::pointer_to_capture(event.position, scale);
        if self.phase == Phase::Active {
            if let Some(id) = self.hit_toolbar_item(self.mouse, scale) {
                self.handle_toolbar(id, window, cx);
                return;
            }
        }
        self.commit_text_draft();
        self.down = true;
        self.down_pt = self.mouse;
        self.hit = SelectionHitKind::None;
        if self.phase == Phase::Active && self.has_sel {
            self.on_active_press();
        }
        cx.notify();
    }

    /// Fold a pending text draft into the document. Any click that leaves the text
    /// field -- canvas or toolbar -- has to run this or the typed text is lost.
    fn commit_text_draft(&mut self) {
        if let Some(edit) = self.text_edit.take() {
            self.editor.add_text(edit.pt, &edit.buf);
        }
    }

    fn on_active_press(&mut self) {
        match self.editor.tool {
            Tool::Select => {
                self.hit = hit_test(self.selection, self.mouse, 8.0);
                self.sel_at_press = self.selection;
                let on_handle =
                    self.hit != SelectionHitKind::None && self.hit != SelectionHitKind::Inside;
                if !on_handle {
                    self.editor.pointer_pressed(self.mouse);
                    if self.editor.selected_id.is_some() {
                        self.hit = SelectionHitKind::None;
                    }
                }
            }
            // Drawing tools only start inside the crop. Clipping alone would let a
            // stroke begin off-frame and appear out of nowhere, and a text caret
            // placed outside would be invisible while the user typed into it.
            _ if !self.selection.contains(self.mouse) => {
                self.down = false;
            }
            _ => match self.editor.pointer_pressed(self.mouse) {
                PressKind::RequestText => {
                    self.down = false;
                    self.text_edit = Some(TextDraft {
                        pt: self.mouse,
                        buf: String::new(),
                    });
                }
                PressKind::Capture => {}
            },
        }
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
            if self.text_edit.take().is_some() {
                cx.notify();
                return;
            }
            self.cancel(window, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.mouse = Self::pointer_to_capture(event.position, window.scale_factor());
        self.on_up();
        cx.notify();
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
            if self.editor.tool != Tool::Select && self.editor.tool != Tool::Text {
                if !self.editor.is_dragging() {
                    let _ = self.editor.pointer_pressed(self.down_pt);
                }
                self.editor.pointer_moved(self.mouse);
                return;
            }
            if self.editor.is_dragging() && self.editor.selected_id.is_some() {
                self.editor.pointer_moved(self.mouse);
                return;
            }
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

    fn on_up(&mut self) {
        if !self.down {
            return;
        }
        self.down = false;
        if self.phase == Phase::Active {
            self.editor.pointer_released();
            return;
        }
        if self.phase == Phase::Selecting {
            let dx = (self.mouse.x - self.down_pt.x).abs();
            let dy = (self.mouse.y - self.down_pt.y).abs();
            if dx <= MIN_RECT && dy <= MIN_RECT {
                if let Some(h) = self.hover {
                    self.selection = h;
                    self.from_drag = false;
                    self.has_sel = true;
                    self.phase = Phase::Active;
                }
            } else if self.has_sel
                && self.selection.width > MIN_RECT
                && self.selection.height > MIN_RECT
            {
                self.phase = Phase::Active;
            }
        }
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
        let (write_file, copy) = complete_export_flags();
        Some(OverlayResult {
            confirmed: true,
            write_file,
            copy,
            x: x as u32,
            y: y as u32,
            w: w as u32,
            h: h as u32,
            marks: self.editor.elements.clone(),
        })
    }

    fn confirm_action(
        &mut self,
        _: &ConfirmSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text_edit.is_some() {
            self.commit_text_draft();
            cx.notify();
            return;
        }
        self.confirm(window, cx);
    }

    fn cancel_action(&mut self, _: &CancelOverlay, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.take().is_some() {
            cx.notify();
            return;
        }
        self.cancel(window, cx);
    }

    fn undo_action(&mut self, _: &UndoEdit, _: &mut Window, cx: &mut Context<Self>) {
        // The draft is not in history, so undoing past it would silently drop the
        // previous annotation while the text field stayed on screen.
        if self.text_edit.take().is_none() {
            self.editor.undo();
        }
        cx.notify();
    }

    fn redo_action(&mut self, _: &RedoEdit, _: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.is_none() {
            self.editor.redo();
        }
        cx.notify();
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        if event.keystroke.key == "backspace" {
            edit.buf.pop();
            cx.notify();
            return;
        }
        if event.keystroke.modifiers.control
            || event.keystroke.modifiers.alt
            || event.keystroke.modifiers.platform
        {
            return;
        }
        if let Some(ch) = event.keystroke.key_char.as_ref() {
            if !ch.is_empty() && ch != "\r" && ch != "\n" && ch != "\u{1b}" {
                edit.buf.push_str(ch);
                cx.notify();
            }
        }
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

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(mut result) = self.result_from_current() else {
            return;
        };
        result.copy = false;
        result.write_file = true;
        if let Some(cap) = self.cap.as_ref() {
            crate::app::export_annotated_region(cx, cap, &result);
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        self.finish(empty_result(), window, cx);
    }

    fn finish(&mut self, result: OverlayResult, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = false;
        // Hide only once the now-empty frame has been committed, so nothing of this
        // capture survives in gpui's swap chain for the next one to flash. Hiding is
        // idempotent, and a full-screen topmost window that never goes away would be
        // far worse than a flash, so a timer hides it regardless if that frame never
        // arrives.
        window.on_next_frame(|window, _cx| {
            let _ = window.wait_for_commit_completion();
            Self::hide_window(window);
        });
        cx.spawn(async move |_this, cx| {
            Timer::after(Duration::from_millis(80)).await;
            let _ = cx.update(|cx| {
                if let Some(handle) = OVERLAY_WINDOW.with(|s| *s.borrow()) {
                    let _ = handle.update(cx, |_view, window, _cx| {
                        Self::hide_window(window);
                    });
                }
            });
        })
        .detach();
        let cap = self.cap.take();
        let perf = self.perf.borrow_mut().take();
        crate::app::complete_region_shot(result, cap, perf, cx);
        cx.notify();
    }

    fn active_monitor_rect(&self) -> Rect {
        let cx = self.origin_x as f64 + self.selection.x + self.selection.width / 2.0;
        let cy = self.origin_y as f64 + self.selection.y + self.selection.height / 2.0;
        if let Ok(mons) = enumerate_monitors() {
            for m in mons {
                if cx >= m.x as f64
                    && cx < (m.x + m.w) as f64
                    && cy >= m.y as f64
                    && cy < (m.y + m.h) as f64
                {
                    return Rect::new(
                        (m.x - self.origin_x) as f64,
                        (m.y - self.origin_y) as f64,
                        m.w as f64,
                        m.h as f64,
                    );
                }
            }
        }
        Rect::new(
            0.0,
            0.0,
            self.freeze_size.0 as f64,
            self.freeze_size.1 as f64,
        )
    }

    fn toolbar_geom(&self, scale: f32) -> Option<Rect> {
        if self.phase != Phase::Active || !self.has_sel {
            return None;
        }
        let n = PHASE1_ITEMS.len();
        let (w, h) = toolbar::bar_size(n);
        let s = scale as f64;
        let pos = toolbar::place_toolbar(
            self.selection,
            w * s,
            h * s,
            self.active_monitor_rect(),
            toolbar::ANCHOR_GAP * s,
        );
        Some(Rect::new(pos.x, pos.y, w * s, h * s))
    }

    fn hit_toolbar_item(&self, p: Point, scale: f32) -> Option<&'static str> {
        let geom = self.toolbar_geom(scale)?;
        let hidden = toolbar::parse_hidden(Some(""));
        let items: Vec<&'static str> = PHASE1_ITEMS
            .iter()
            .copied()
            .filter(|id| toolbar::is_visible(id, &hidden))
            .collect();
        toolbar_item_at(p, geom, &items)
    }

    fn handle_toolbar(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        // Buttons stop propagation, so `on_mouse_down` never runs for these clicks.
        // Undo/redo/cancel discard the draft instead, matching their keyboard paths.
        if matches!(id, toolbar::UNDO | toolbar::REDO | toolbar::CANCEL) {
            if self.text_edit.take().is_some() {
                cx.notify();
                return;
            }
        } else {
            self.commit_text_draft();
        }
        if let Some(tool) = Tool::from_id(id) {
            self.editor.cancel_draft();
            self.editor.tool = tool;
            cx.notify();
            return;
        }
        match id {
            toolbar::UNDO => {
                self.editor.undo();
                cx.notify();
            }
            toolbar::REDO => {
                self.editor.redo();
                cx.notify();
            }
            toolbar::CLEAR => {
                self.editor.clear();
                cx.notify();
            }
            toolbar::SAVE => self.save(cx),
            toolbar::COMPLETE => self.confirm(window, cx),
            toolbar::CANCEL => self.cancel(window, cx),
            _ => {}
        }
    }

    /// Per-element coverage masks, cached against the exact element list. Preview
    /// and export rasterize from the same masks (`composite::element_mask`), so
    /// this cache is the only thing standing between the ants animation and
    /// re-rasterizing every stroke at 60 fps.
    fn mark_masks(&self, marks: &[Element]) -> Vec<Option<Rc<CoverageMask>>> {
        if let Some((key, masks)) = self.mask_cache.borrow().as_ref() {
            if key.as_slice() == marks {
                return masks.clone();
            }
        }
        let masks: Vec<Option<Rc<CoverageMask>>> = marks
            .iter()
            .map(|el| element_mask(el).map(Rc::new))
            .collect();
        *self.mask_cache.borrow_mut() = Some((marks.to_vec(), masks.clone()));
        masks
    }

    /// Pixelated tiles plus the ids of the elements that produced them; an element
    /// missing from that list still needs a placeholder drawn for it.
    /// Pixelated tiles plus the ids of the elements that produced them; an element
    /// missing from that list still needs a placeholder drawn for it.
    fn mosaic_tiles(&self, cx: &mut App) -> MosaicTiles {
        let mut out = MosaicTiles::default();
        let Some(cap) = self.cap.as_ref() else {
            return out;
        };
        let key: MosaicKey = self
            .editor
            .visible()
            .filter_map(|el| match &el.shape {
                Shape::Mosaic { points, block_size } => {
                    Some((el.id, el.bounds, *block_size, points.len()))
                }
                _ => None,
            })
            .collect();
        if let Some((cached_key, tiles)) = self.mosaic_cache.borrow().as_ref() {
            if *cached_key == key {
                return tiles.clone();
            }
        }
        for el in self.editor.visible() {
            let Shape::Mosaic { points, block_size } = &el.shape else {
                continue;
            };
            let block = (*block_size).max(2);
            let cells = crate::overlay::editor::mosaic_cells(points, block);
            if cells.is_empty() {
                continue;
            }
            // Read back one grid-aligned rect covering every cell, then average
            // inside it. Per-cell readbacks would be one GPU round trip per tile.
            // ponytail: every new path point invalidates the cache and re-reads the
            // whole stroke bounds. Fine up to a few hundred cells; if smudging across
            // a full screen drops frames, cache per element id and only read back the
            // cells the newest segment added.
            let gx0 = cells.iter().map(|c| c.0).min().unwrap_or(0);
            let gy0 = cells.iter().map(|c| c.1).min().unwrap_or(0);
            let gx1 = cells.iter().map(|c| c.0).max().unwrap_or(0) + 1;
            let gy1 = cells.iter().map(|c| c.1).max().unwrap_or(0) + 1;
            let x = (gx0 * block).clamp(0, cap.width as i32) as u32;
            let y = (gy0 * block).clamp(0, cap.height as i32) as u32;
            let w = (gx1 * block).clamp(0, cap.width as i32) as u32 - x;
            let h = (gy1 * block).clamp(0, cap.height as i32) as u32 - y;
            if w < 1 || h < 1 {
                continue;
            }
            let Ok((px, cw, ch, stride)) = (match cx
                .with_gpu_context(|_d, ctx| crop_sdr_bgra(cap, ctx, x, y, w, h))
            {
                Some(r) => r,
                None => continue,
            }) else {
                continue;
            };
            for (gx, gy) in cells {
                let cx0 = (gx * block - x as i32).clamp(0, cw as i32);
                let cy0 = (gy * block - y as i32).clamp(0, ch as i32);
                let cx1 = (gx * block + block - x as i32).clamp(0, cw as i32);
                let cy1 = (gy * block + block - y as i32).clamp(0, ch as i32);
                if cx1 <= cx0 || cy1 <= cy0 {
                    continue;
                }
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for sy in cy0..cy1 {
                    for sx in cx0..cx1 {
                        let i = (sy as u32 * stride + sx as u32 * 4) as usize;
                        if i + 2 >= px.len() {
                            continue;
                        }
                        b += px[i] as u32;
                        g += px[i + 1] as u32;
                        r += px[i + 2] as u32;
                        n += 1;
                    }
                }
                if n == 0 {
                    continue;
                }
                let rgb = ((r / n) << 16) | ((g / n) << 8) | (b / n);
                out.tiles.push((
                    Rect::new(
                        (gx * block) as f64,
                        (gy * block) as f64,
                        block as f64,
                        block as f64,
                    ),
                    rgb,
                ));
            }
            out.covered.push(el.id);
        }
        *self.mosaic_cache.borrow_mut() = Some((key, out.clone()));
        out
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

    fn toolbar_element(&self, scale: f32, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let geom = self.toolbar_geom(scale)?;
        let tool_id = self.editor.tool.id();
        let can_undo = self.editor.history.can_undo();
        let can_redo = self.editor.history.can_redo();
        let hidden = toolbar::parse_hidden(Some(""));
        Some(
            div()
                .absolute()
                .left(px(geom.x as f32 / scale))
                .top(px(geom.y as f32 / scale))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(toolbar::ITEM_GAP as f32))
                .px(px(toolbar::PAD as f32))
                .py(px(toolbar::PAD as f32))
                .rounded_xl()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .shadow(vec![BoxShadow {
                    color: theme::base().opacity(0.18),
                    offset: point(px(0.), px(4.)),
                    blur_radius: px(16.),
                    spread_radius: px(0.),
                }])
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .children(PHASE1_ITEMS.iter().copied().filter(|id| toolbar::is_visible(id, &hidden)).map(|id| {
                    let active = tool_id == id;
                    let dim = (id == toolbar::UNDO && !can_undo)
                        || (id == toolbar::REDO && !can_redo);
                    toolbar_button(id, active, dim, cx)
                })),
        )
    }

    /// Text annotations live in the element tree, not the canvas, so the paint-time
    /// content mask does not reach them. Same clip, done with a container: positioned
    /// on the selection, children placed relative to it, overflow hidden.
    fn text_layer(&self, scale: f32) -> Option<impl IntoElement> {
        if !self.has_sel {
            return None;
        }
        let sel = self.selection;
        let committed: Vec<_> = self
            .editor
            .elements
            .iter()
            .filter_map(|el| {
                let Shape::Text { text, font_size } = &el.shape else {
                    return None;
                };
                Some(
                    div()
                        .absolute()
                        .left(px((el.bounds.x - sel.x) as f32 / scale))
                        .top(px((el.bounds.y - sel.y) as f32 / scale))
                        .text_color(rgb(el.stroke & 0x00FF_FFFF))
                        .text_size(px(*font_size as f32 / scale))
                        .child(text.clone()),
                )
            })
            .collect();
        let draft = self.text_edit.as_ref().map(|edit| {
            let shown = if edit.buf.is_empty() {
                "|".to_string()
            } else {
                format!("{}|", edit.buf)
            };
            div()
                .absolute()
                .left(px((edit.pt.x - sel.x) as f32 / scale))
                .top(px((edit.pt.y - sel.y) as f32 / scale))
                .min_w(px(80.))
                .px(px(4.))
                .py(px(2.))
                .bg(white())
                .text_color(black())
                .text_sm()
                .occlude()
                .child(shown)
        });
        Some(
            div()
                .absolute()
                .left(px(sel.x as f32 / scale))
                .top(px(sel.y as f32 / scale))
                .w(px(sel.width as f32 / scale))
                .h(px(sel.height as f32 / scale))
                .overflow_hidden()
                .children(committed)
                .children(draft),
        )
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
        let marks: Vec<Element> = self.editor.visible().cloned().collect();
        let masks = self.mark_masks(&marks);
        let mosaic = self.mosaic_tiles(cx);
        let tool = self.editor.tool;
        let cursor = match (phase, tool, self.text_edit.is_some()) {
            (_, _, true) => CursorStyle::IBeam,
            (Phase::Selecting, _, _) => CursorStyle::Crosshair,
            (Phase::Active, Tool::Select, _) => CursorStyle::Arrow,
            (Phase::Active, Tool::Text, _) => CursorStyle::IBeam,
            _ => CursorStyle::Crosshair,
        };

        div()
            .id("overlay-root")
            .size_full()
            .key_context("Overlay")
            .track_focus(&self.focus_handle)
            .cursor(cursor)
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::confirm_action))
            .on_action(cx.listener(Self::cancel_action))
            .on_action(cx.listener(Self::undo_action))
            .on_action(cx.listener(Self::redo_action))
            // Everything visible hangs off `visible`, so the frame drawn on the way
            // out is empty. gpui's layer is a separate swap chain from the freeze
            // frame and keeps its last frame while the window is hidden; leaving the
            // finished overlay in it means the next capture shows those pixels until
            // the first repaint lands.
            .when(self.visible, |el| {
                el.child(
                    canvas(
                        move |_, _, _| (),
                        move |bounds, _, window, _cx| {
                            paint_overlay(
                                window,
                                bounds,
                                OverlayPaint {
                                    shown,
                                    mouse,
                                    mag,
                                    phase,
                                    start,
                                    has_sel,
                                    scale,
                                    marks,
                                    masks,
                                    mosaic,
                                },
                            );
                        },
                    )
                    .size_full(),
                )
                .child(coord_label(shown, mouse, phase, scale))
                .children(self.toolbar_element(scale, cx))
                .children(self.text_layer(scale))
            })
    }
}

/// Hover label for a toolbar button. gpui owns the show delay and placement; this
/// only has to render the bubble.
struct ToolLabel(SharedString);

impl Render for ToolLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(6.))
            .py(px(3.))
            .rounded_md()
            .bg(theme::surface())
            .border_1()
            .border_color(theme::border())
            .text_color(theme::primary_fg())
            .text_size(px(12.))
            .child(self.0.clone())
    }
}

fn toolbar_button(
    id: &'static str,
    active: bool,
    dim: bool,
    cx: &mut Context<OverlayView>,
) -> impl IntoElement {
    let fg = if dim {
        theme::disabled_fg()
    } else if active {
        theme::accent()
    } else {
        theme::primary_fg()
    };
    let idle_bg = if active {
        theme::active_background()
    } else {
        theme::transparent()
    };
    let hover_bg = if active {
        theme::active_background()
    } else {
        theme::elevated_surface()
    };
    div()
        .id(id)
        .w(px(toolbar::BTN as f32))
        .h(px(toolbar::BTN as f32))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(idle_bg)
        .text_color(fg)
        .cursor(if dim {
            CursorStyle::Arrow
        } else {
            CursorStyle::PointingHand
        })
        .when(!dim, |el| el.hover(move |s| s.bg(hover_bg)))
        .child(
            svg()
                .path(toolbar::icon_path(id).unwrap_or(""))
                .w(px(toolbar::ICON))
                .h(px(toolbar::ICON))
                .text_color(fg),
        )
        .tooltip(move |_, cx| {
            let view: AnyView = cx.new(|_| ToolLabel(toolbar::label(id).into())).into();
            view
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, _, window, cx| {
                view.handle_toolbar(id, window, cx);
                cx.stop_propagation();
            }),
        )
}

/// Mirror the rendered layout -- `PAD`, then one `BTN` every `BTN + ITEM_GAP`.
/// Splitting `geom.width` into equal slots instead drifts by the padding and can
/// land a click on Cancel in Complete's slot.
fn toolbar_item_at(p: Point, geom: Rect, items: &[&'static str]) -> Option<&'static str> {
    if items.is_empty() || !geom.contains(p) {
        return None;
    }
    let s = geom.width / toolbar::bar_size(items.len()).0;
    let pitch = (toolbar::BTN + toolbar::ITEM_GAP) * s;
    if pitch <= 0.0 {
        return None;
    }
    let rel = p.x - geom.x - toolbar::PAD * s;
    if rel < 0.0 {
        return None;
    }
    let i = (rel / pitch).floor();
    if rel - i * pitch > toolbar::BTN * s {
        return None;
    }
    items.get(i as usize).copied()
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
    marks: Vec<Element>,
    masks: Vec<Option<Rc<CoverageMask>>>,
    mosaic: MosaicTiles,
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
        marks,
        masks,
        mosaic,
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
    // Annotations are clipped to the selection: the export composites onto a buffer
    // that IS the crop, so anything outside it never reaches the saved image. Without
    // the same clip here the overlay would promise strokes the file does not have.
    match shown {
        Some(r) => {
            let mask = ContentMask {
                bounds: rect_to_bounds(r, scale),
            };
            window.with_content_mask(Some(mask), |window| {
                paint_marks(window, &marks, &masks, &mosaic, scale);
            });
        }
        None => paint_marks(window, &marks, &masks, &mosaic, scale),
    }
    if phase == Phase::Selecting {
        if let Some(pixels) = mag {
            paint_magnifier(window, bounds, mouse, pixels, scale);
        }
    }
}

fn paint_marks(
    window: &mut Window,
    marks: &[Element],
    masks: &[Option<Rc<CoverageMask>>],
    mosaic: &MosaicTiles,
    scale: f32,
) {
    for (r, color) in &mosaic.tiles {
        window.paint_quad(fill(rect_to_bounds(*r, scale), rgb(*color)));
    }
    for (el, mask) in marks.iter().zip(masks.iter()) {
        match &el.shape {
            Shape::Mosaic { .. } => {
                if !mosaic.covered.contains(&el.id) {
                    window.paint_quad(fill(
                        rect_to_bounds(el.bounds, scale),
                        hsla(0., 0., 0.5, 0.35),
                    ));
                }
            }
            // Text is a GPUI div in the overlay tree; CPU composite handles export.
            Shape::Text { .. } => {}
            _ => {
                if let Some(mask) = mask {
                    paint_mask(window, mask, scale, rgb(el.stroke & 0x00FF_FFFF));
                }
            }
        }
    }
}

/// Paint one element's coverage mask as alpha-weighted quads, one per
/// constant-coverage run of physical pixels.
///
/// The mask IS the per-pixel coverage the export composite blends
/// (`composite::element_mask`), so the preview cannot drift from the saved
/// image. Each run becomes a quad whose logical coordinates are physical/scale,
/// which `paint_quad` multiplies back by `scale_factor`: at 100%/125%/.../200%
/// DPI every quad lands exactly on whole device pixels, and the coverage alpha
/// supplies the antialiasing the quad pipeline itself does not have (plain
/// filled quads take a hard-edged fast path in the DirectX shader). Runs are
/// disjoint, so overlapping segments never double-blend -- the mask already
/// resolved them with max.
fn paint_mask(window: &mut Window, mask: &CoverageMask, scale: f32, color: gpui::Rgba) {
    for run in mask.runs() {
        let mut c = color;
        c.a *= run.alpha as f32 / 255.0;
        window.paint_quad(fill(
            Bounds {
                origin: point(px(run.x0 as f32 / scale), px(run.y as f32 / scale)),
                size: size(px((run.x1 - run.x0) as f32 / scale), px(1.0 / scale)),
            },
            c,
        ));
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

/// Outer HWND rect whose client area equals `desired` (screen coordinates).
/// GPUI PopUp `WM_NCCALCSIZE` insets would otherwise shift the freeze visual right.
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
    apply_nc_borders(window, origin, client, desired)
}

fn apply_nc_borders(window: RECT, client_origin: POINT, client: RECT, desired: RECT) -> RECT {
    let left = client_origin.x - window.left;
    let top = client_origin.y - window.top;
    let right = window.right - (client_origin.x + client.right - client.left);
    let bottom = window.bottom - (client_origin.y + client.bottom - client.top);
    RECT {
        left: desired.left - left,
        top: desired.top - top,
        right: desired.right + right,
        bottom: desired.bottom + bottom,
    }
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

    #[test]
    fn nc_insets_expand_window_so_client_matches_capture() {
        // Win11-style: 8px left/right/bottom, 1px top.
        let window = RECT {
            left: 100,
            top: 50,
            right: 100 + 1920 + 16,
            bottom: 50 + 1080 + 9,
        };
        let client_origin = POINT { x: 108, y: 51 };
        let client = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let desired = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let outer = apply_nc_borders(window, client_origin, client, desired);
        assert_eq!(outer.left, -8);
        assert_eq!(outer.top, -1);
        assert_eq!(outer.right, 1928);
        assert_eq!(outer.bottom, 1088);
    }

    #[test]
    fn zero_nc_insets_keep_window_on_capture_bounds() {
        let window = RECT {
            left: 0,
            top: 0,
            right: 800,
            bottom: 600,
        };
        let client_origin = POINT { x: 0, y: 0 };
        let client = RECT {
            left: 0,
            top: 0,
            right: 800,
            bottom: 600,
        };
        let desired = RECT {
            left: -1920,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let outer = apply_nc_borders(window, client_origin, client, desired);
        assert_eq!(outer.left, desired.left);
        assert_eq!(outer.top, desired.top);
        assert_eq!(outer.right, desired.right);
        assert_eq!(outer.bottom, desired.bottom);
    }

    #[test]
    fn complete_defaults_to_clipboard_not_file() {
        let (write_file, copy) = complete_export_flags();
        assert!(!write_file);
        assert!(copy);
    }

    #[test]
    fn toolbar_item_at_picks_slot_under_pointer() {
        let items = ["select", "rect", "arrow", "pen"];
        let (w, h) = toolbar::bar_size(items.len());
        let geom = Rect::new(100.0, 200.0, w, h);
        // Button i spans [100 + PAD + i*38, +BTN] = [104,140] [142,178] [180,216] [218,254].
        let at = |x: f64| toolbar_item_at(Point { x, y: 210.0 }, geom, &items);
        assert_eq!(at(104.0), Some("select"));
        assert_eq!(at(139.0), Some("select"));
        assert_eq!(at(142.0), Some("rect"));
        assert_eq!(at(215.0), Some("arrow"));
        assert_eq!(at(253.0), Some("pen"));
        assert_eq!(at(101.0), None, "left padding is not a button");
        assert_eq!(at(141.0), None, "gap between buttons is not a button");
        assert_eq!(at(50.0), None);
        assert_eq!(toolbar_item_at(Point { x: 210.0, y: 10.0 }, geom, &items), None);
    }

    /// The whole point of the rewrite: the last slot must not steal Cancel's clicks.
    #[test]
    fn toolbar_item_at_matches_rendered_buttons_for_the_full_bar() {
        let items: Vec<&'static str> = PHASE1_ITEMS.to_vec();
        let (w, h) = toolbar::bar_size(items.len());
        let geom = Rect::new(0.0, 0.0, w, h);
        for (i, id) in items.iter().enumerate() {
            let left = toolbar::PAD + i as f64 * (toolbar::BTN + toolbar::ITEM_GAP);
            for x in [left, left + toolbar::BTN / 2.0, left + toolbar::BTN - 0.1] {
                assert_eq!(
                    toolbar_item_at(Point { x, y: 20.0 }, geom, &items),
                    Some(*id),
                    "x={x} should hit {id}"
                );
            }
        }
    }
}

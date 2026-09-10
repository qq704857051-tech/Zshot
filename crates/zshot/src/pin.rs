//! Pin-to-screen: the annotated crop becomes a topmost, borderless window that
//! stays on the desktop after the overlay closes (PixPin 钉图). Display, drag,
//! wheel zoom, Esc / double-click to close. Any number of pin windows can be
//! alive at once; the registry below keeps their handles so the windows are
//! not dropped.
//!
//! Stitching (拼接): dragging a lone pin near the exposed edge of another pin
//! snaps it flush and shows a ✓ on the seam. Confirming links the two into a
//! *group*: tiles that move, zoom and close together, and are composited into
//! one image on copy / save. Groups are never rasterized while on screen, so
//! a staircase-shaped group really is a set of rectangular windows and the
//! notches stay click-through. Alt while dragging bypasses snapping; Ctrl+D
//! detaches a tile from its group.
//!
//! Annotating (标注, Ctrl+E or the context menu) opens the drawing tools over
//! the whole group; see `pin_edit`. Right-click posts to the hidden window,
//! which shows a native popup menu and calls back with the chosen command.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use gpui::{
    actions, canvas, div, hsla, img, prelude::*, px, size, white, App, Bounds, Context,
    CursorStyle, DispatchPhase, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable,
    ImageSource, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point as GpuiPoint, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    UTF16Selection, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, CreatePopupMenu, DeferWindowPos, DestroyMenu, EndDeferWindowPos,
    GetClientRect, GetCursorPos, GetWindowRect, InsertMenuW, PostMessageW, SetForegroundWindow,
    SetWindowPos, TrackPopupMenu, HWND_TOPMOST, MF_BYPOSITION, MF_SEPARATOR, MF_STRING,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

use crate::geom::{Point, Rect};
use crate::overlay::annotate::{self, TextDraft, TextLayer};
use crate::overlay::editor::{Element, PressKind, Tool};
use crate::pin_edit;
use crate::pin_snap::{
    are_adjacent, compose, find_snap, flatten_gaps, rounded_rect, zoom_rect, IRect, Side,
    SnapRules, Tile, TileImage,
};
use crate::platform::{hook_target, WM_ZSHOT_PIN_MENU};

actions!(
    pin,
    [
        ClosePin,
        ConfirmStitch,
        CopyPin,
        SavePin,
        DetachPin,
        AnnotatePin,
        UndoPin,
        RedoPin
    ]
);

/// One live pin window. `group` is shared by every tile of a stitched group
/// and equals `id` for a lone pin.
#[derive(Clone, Copy)]
struct PinEntry {
    id: u64,
    group: u64,
    hwnd: HWND,
    handle: WindowHandle<PinView>,
}

thread_local! {
    static PINS: RefCell<Vec<PinEntry>> = const { RefCell::new(Vec::new()) };
    static KEYS_BOUND: Cell<bool> = const { Cell::new(false) };
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 10.0;
const ZOOM_STEP: f32 = 1.1;

/// Snap engages inside this gap and lets go only past `SNAP_EXIT`, so the
/// window does not flicker on the boundary.
const SNAP_ENTER: i32 = 12;
const SNAP_EXIT: i32 = 20;
const MIN_SHARED_EDGE: i32 = 16;
const MAX_OVERLAP: f32 = 0.5;
/// Below this logical size the ✓ and ✕ buttons would cover the image.
const BUTTON_MIN_SIDE: f32 = 48.0;

fn snap_rules(tolerance: i32) -> SnapRules {
    SnapRules {
        tolerance,
        min_shared: MIN_SHARED_EDGE,
        max_overlap: MAX_OVERLAP,
    }
}

/// What compositing and annotating need to know about one tile.
pub(crate) struct TileSnapshot {
    pub id: u64,
    /// Client rect, screen physical px.
    pub rect: IRect,
    pub precise_rect: Rect,
    /// Tight BGRA at native size.
    pub pixels: Arc<Vec<u8>>,
    pub size: (u32, u32),
    pub zoom: f32,
}

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
    let pixels = Arc::new(buf);
    let Some(image) = render_image(&pixels, w, h) else {
        crate::perf::log_app("pin: pixel buffer size mismatch");
        return;
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

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
        |window, cx| cx.new(|cx| PinView::new(cx, id, image, pixels, w, h, window)),
    ) {
        Ok(handle) => handle,
        Err(e) => {
            crate::perf::log_app(&format!("pin window failed: {e}"));
            return;
        }
    };

    let hwnd = handle.update(cx, |view, window, cx| {
        view.attach_to_screen(window, screen_x, screen_y);
        window.focus(&view.focus_handle);
        window.on_window_should_close(cx, move |_, cx| {
            forget(&[id]);
            cx.refresh_windows();
            true
        });
        window.win32_hwnd()
    });
    let Ok(hwnd) = hwnd else {
        return;
    };
    PINS.with(|slot| {
        slot.borrow_mut().push(PinEntry {
            id,
            group: id,
            hwnd,
            handle,
        })
    });
}

fn render_image(pixels: &Arc<Vec<u8>>, w: u32, h: u32) -> Option<Arc<RenderImage>> {
    let rgba = image::RgbaImage::from_raw(w, h, pixels.as_ref().clone())?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

fn bind_pin_keys(cx: &mut App) {
    if KEYS_BOUND.get() {
        return;
    }
    cx.bind_keys(vec![
        KeyBinding::new("escape", ClosePin, Some("Pin")),
        KeyBinding::new("enter", ConfirmStitch, Some("Pin")),
        KeyBinding::new("ctrl-c", CopyPin, Some("Pin")),
        KeyBinding::new("ctrl-s", SavePin, Some("Pin")),
        KeyBinding::new("ctrl-d", DetachPin, Some("Pin")),
        KeyBinding::new("ctrl-e", AnnotatePin, Some("Pin")),
        KeyBinding::new("ctrl-z", UndoPin, Some("Pin")),
        KeyBinding::new("ctrl-y", RedoPin, Some("Pin")),
    ]);
    KEYS_BOUND.set(true);
}

// ---- registry -------------------------------------------------------------

fn entries() -> Vec<PinEntry> {
    PINS.with(|slot| slot.borrow().clone())
}

fn entry(id: u64) -> Option<PinEntry> {
    PINS.with(|slot| slot.borrow().iter().copied().find(|e| e.id == id))
}

pub(crate) fn entry_handle(id: u64) -> Option<WindowHandle<PinView>> {
    entry(id).map(|e| e.handle)
}

fn group_of(id: u64) -> Option<u64> {
    entry(id).map(|e| e.group)
}

fn group_entries(group: u64) -> Vec<PinEntry> {
    entries().into_iter().filter(|e| e.group == group).collect()
}

/// Other tiles of `id`'s group, `id` itself excluded.
fn group_members(id: u64) -> Vec<PinEntry> {
    let Some(me) = entry(id) else {
        return Vec::new();
    };
    entries()
        .into_iter()
        .filter(|e| e.group == me.group && e.id != id)
        .collect()
}

/// Merges `a`'s group into `b`'s.
fn link(a: u64, b: u64) {
    let (Some(ea), Some(eb)) = (entry(a), entry(b)) else {
        return;
    };
    PINS.with(|slot| {
        for e in slot.borrow_mut().iter_mut() {
            if e.group == ea.group {
                e.group = eb.group;
            }
        }
    });
}

/// Moves `id` into a fresh group of its own. A new number rather than `id`:
/// the group may currently be *named* by this pin's id and the others must
/// keep it.
fn detach(id: u64) {
    let fresh = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    PINS.with(|slot| {
        for e in slot.borrow_mut().iter_mut() {
            if e.id == id {
                e.group = fresh;
            }
        }
    });
}

fn forget(ids: &[u64]) {
    PINS.with(|slot| slot.borrow_mut().retain(|e| !ids.contains(&e.id)));
}

fn client_rect(hwnd: HWND) -> Option<IRect> {
    let mut origin = POINT::default();
    let mut client = RECT::default();
    unsafe {
        if !ClientToScreen(hwnd, &mut origin).as_bool() || GetClientRect(hwnd, &mut client).is_err()
        {
            return None;
        }
    }
    Some(IRect::from_origin_size(
        origin.x,
        origin.y,
        client.right - client.left,
        client.bottom - client.top,
    ))
}

fn outer_rect(hwnd: HWND) -> Option<IRect> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect).ok()?;
    }
    Some(IRect::new(rect.left, rect.top, rect.right, rect.bottom))
}

/// Moves a set of windows by one delta in a single batch, so a group never
/// tears apart mid-frame.
fn move_windows(hwnds: &[HWND], dx: i32, dy: i32) {
    if hwnds.is_empty() || (dx == 0 && dy == 0) {
        return;
    }
    unsafe {
        let Ok(mut hdwp) = BeginDeferWindowPos(hwnds.len() as i32) else {
            return;
        };
        for &hwnd in hwnds {
            let Some(outer) = outer_rect(hwnd) else {
                continue;
            };
            match DeferWindowPos(
                hdwp,
                hwnd,
                None,
                outer.left + dx,
                outer.top + dy,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            ) {
                Ok(next) => hdwp = next,
                Err(_) => return,
            }
        }
        let _ = EndDeferWindowPos(hdwp);
    }
}

fn set_client_rect(hwnd: HWND, desired: IRect) {
    let outer = window_rect_for_client_area(hwnd, desired);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            outer.left,
            outer.top,
            outer.width(),
            outer.height(),
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
    }
}

/// Repaints every tile of `group` and its toolbar, if editing. A window that
/// is currently mid-update (the caller's own) is skipped; it notifies itself.
pub(crate) fn refresh_group(cx: &mut App, group: u64) {
    for e in group_entries(group) {
        let _ = e.handle.update(cx, |_, _, cx| cx.notify());
    }
    if let Some(edit) = pin_edit::edit_for_group(group) {
        let toolbar = edit.borrow().toolbar_handle();
        if let Some(t) = toolbar {
            let _ = t.update(cx, |_, _, cx| cx.notify());
        }
    }
}

/// Snapshots of every tile in `group` read through their windows; `mine` is
/// the caller's own tile, which cannot be read while it is being updated.
pub(crate) fn snapshots(cx: &mut App, group: u64, mine: Option<TileSnapshot>) -> Vec<TileSnapshot> {
    let skip = mine.as_ref().map(|t| t.id);
    let mut out: Vec<TileSnapshot> = mine.into_iter().collect();
    for e in group_entries(group) {
        if Some(e.id) == skip {
            continue;
        }
        let (Ok(view), Some(rect)) = (e.handle.read(cx), client_rect(e.hwnd)) else {
            continue;
        };
        out.push(TileSnapshot {
            id: e.id,
            rect,
            precise_rect: view.precise_rect(rect),
            pixels: view.pixels.clone(),
            size: view.pixel_size,
            zoom: view.zoom,
        });
    }
    out
}

// ---- view -----------------------------------------------------------------

/// A snap that is showing its ✓: `target` is the pin this one is flush with.
#[derive(Clone, Copy)]
struct Pending {
    target: u64,
    side: Side,
}

pub struct PinView {
    id: u64,
    image: Arc<RenderImage>,
    /// Tight BGRA at native size, kept for compositing a group.
    pixels: Arc<Vec<u8>>,
    /// Physical pixel size of the flattened crop.
    pixel_size: (u32, u32),
    zoom: f32,
    layout_rect: Option<Rect>,
    /// Cursor position minus the window origin at drag start, screen coords.
    drag_offset: Option<(i32, i32)>,
    pending: Option<Pending>,
    /// A stroke in progress started on this tile (edit mode).
    edit_down: Option<Point>,
    focus_handle: FocusHandle,
}

impl PinView {
    fn new(
        cx: &mut Context<Self>,
        id: u64,
        image: Arc<RenderImage>,
        pixels: Arc<Vec<u8>>,
        w: u32,
        h: u32,
        _window: &mut Window,
    ) -> Self {
        Self {
            id,
            image,
            pixels,
            pixel_size: (w.max(1), h.max(1)),
            zoom: 1.0,
            layout_rect: None,
            drag_offset: None,
            pending: None,
            edit_down: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Exact placement: GPUI's window_bounds are logical pixels, and the
    /// caption-free window still carries non-client insets, so set the outer
    /// rect whose client area matches the crop at its original screen position.
    fn attach_to_screen(&mut self, window: &mut Window, screen_x: i32, screen_y: i32) {
        let hwnd = window.win32_hwnd();
        let desired = IRect::from_origin_size(
            screen_x,
            screen_y,
            self.pixel_size.0 as i32,
            self.pixel_size.1 as i32,
        );
        let outer = window_rect_for_client_area(hwnd, desired);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                outer.left,
                outer.top,
                outer.width(),
                outer.height(),
                SWP_SHOWWINDOW,
            );
        }
    }

    fn group(&self) -> u64 {
        group_of(self.id).unwrap_or(self.id)
    }

    fn editing(&self) -> bool {
        pin_edit::is_editing(self.group())
    }

    /// Swaps in new native pixels (after 完成 flattens the marks).
    pub(crate) fn replace_pixels(&mut self, pixels: Vec<u8>, w: u32, h: u32, zoom: f32) {
        let pixels = Arc::new(pixels);
        let Some(image) = render_image(&pixels, w, h) else {
            return;
        };
        self.pixels = pixels;
        self.image = image;
        self.pixel_size = (w.max(1), h.max(1));
        self.zoom = zoom;
        if let Some(rect) = self.layout_rect.as_mut() {
            rect.width = w as f64 * zoom as f64;
            rect.height = h as f64 * zoom as f64;
        }
    }

    fn snapshot(&self, window: &Window) -> Option<TileSnapshot> {
        let rect = client_rect(window.win32_hwnd())?;
        Some(TileSnapshot {
            id: self.id,
            rect,
            precise_rect: self.precise_rect(rect),
            pixels: self.pixels.clone(),
            size: self.pixel_size,
            zoom: self.zoom,
        })
    }

    fn precise_rect(&self, client: IRect) -> Rect {
        let mut rect = self.layout_rect.unwrap_or_else(|| client.precise());
        let rounded = rounded_rect(rect);
        // Dragging translates HWNDs in whole pixels without discarding residuals.
        rect.x += (client.left - rounded.left) as f64;
        rect.y += (client.top - rounded.top) as f64;
        rect
    }

    // ---- close / detach / stitch -------------------------------------------

    /// Esc: drops a text draft, else cancels an edit, else closes the group.
    fn close(&mut self, _: &ClosePin, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            let group = self.group();
            let dropped = pin_edit::edit_for_group(group)
                .is_some_and(|edit| edit.borrow_mut().text.take().is_some());
            if !dropped {
                pin_edit::finish(cx, group, false, Vec::new());
            }
            refresh_group(cx, group);
            cx.notify();
            return;
        }
        self.close_group(window, cx);
    }

    fn close_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group();
        if self.editing() {
            pin_edit::finish(cx, group, false, Vec::new());
        }
        let members = group_members(self.id);
        let mut ids = vec![self.id];
        for m in &members {
            ids.push(m.id);
            let _ = m.handle.update(cx, |_, window, _| window.remove_window());
        }
        forget(&ids);
        clear_pendings_targeting(cx, &ids, self.id);
        window.remove_window();
    }

    fn detach(&mut self, _: &DetachPin, _: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            return;
        }
        let former = group_members(self.id);
        detach(self.id);
        refresh(cx, &former);
        cx.notify();
    }

    /// Enter: commits a text draft, else finishes an edit, else confirms a
    /// pending stitch.
    fn confirm(&mut self, _: &ConfirmStitch, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            let group = self.group();
            let committed = pin_edit::edit_for_group(group).is_some_and(|edit| {
                let e = &mut *edit.borrow_mut();
                let had = e.text.is_some();
                annotate::commit_text_draft(&mut e.session, &mut e.text);
                had
            });
            if committed {
                refresh_group(cx, group);
                cx.notify();
            } else {
                self.finish_edit(true, window, cx);
            }
            return;
        }
        self.confirm_stitch(window, cx);
    }

    /// Links this pin to the snap target if the two are still flush.
    fn confirm_stitch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let target_editing = group_of(p.target).is_some_and(pin_edit::is_editing);
        let flush = entry(p.target)
            .and_then(|t| Some((client_rect(t.hwnd)?, client_rect(window.win32_hwnd())?)))
            .is_some_and(|(a, b)| are_adjacent(a, b, MIN_SHARED_EDGE));
        if flush && !target_editing {
            if let Some(target) = entry(p.target) {
                if let (Ok(view), Some(other), Some(me)) = (
                    target.handle.read(cx),
                    client_rect(target.hwnd),
                    client_rect(window.win32_hwnd()),
                ) {
                    let target = view.precise_rect(other);
                    let mut rect = self.precise_rect(me);
                    match p.side {
                        Side::Right => rect.x = target.right(),
                        Side::Left => rect.x = target.x - rect.width,
                        Side::Bottom => rect.y = target.bottom(),
                        Side::Top => rect.y = target.y - rect.height,
                    }
                    if matches!(p.side, Side::Right | Side::Left) {
                        if me.top == other.top {
                            rect.y = target.y;
                        } else if me.bottom == other.bottom {
                            rect.y = target.bottom() - rect.height;
                        }
                    } else if me.left == other.left {
                        rect.x = target.x;
                    } else if me.right == other.right {
                        rect.x = target.right() - rect.width;
                    }
                    self.layout_rect = Some(rect);
                    set_client_rect(window.win32_hwnd(), rounded_rect(rect));
                }
            }
            link(self.id, p.target);
            // Membership decides which tile carries the ✕.
            refresh(cx, &group_members(self.id));
        }
        cx.notify();
    }

    // ---- annotate -----------------------------------------------------------

    fn annotate(&mut self, _: &AnnotatePin, window: &mut Window, cx: &mut Context<Self>) {
        self.start_edit(window, cx);
    }

    fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            return;
        }
        self.pending = None;
        self.drag_offset = None;
        let group = self.group();
        let tiles = snapshots(cx, group, self.snapshot(window));
        pin_edit::start(cx, group, &tiles);
        window.focus(&self.focus_handle);
        cx.notify();
    }

    fn finish_edit(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group();
        let tiles = if commit {
            snapshots(cx, group, self.snapshot(window))
        } else {
            Vec::new()
        };
        for (id, px, w, h, zoom) in pin_edit::finish(cx, group, commit, tiles) {
            if id == self.id {
                self.replace_pixels(px, w, h, zoom);
            }
        }
        self.edit_down = None;
        cx.notify();
    }

    fn undo(&mut self, _: &UndoPin, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_history(cx, true);
    }

    fn redo(&mut self, _: &RedoPin, _: &mut Window, cx: &mut Context<Self>) {
        self.edit_history(cx, false);
    }

    fn edit_history(&mut self, cx: &mut Context<Self>, undo: bool) {
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        {
            let mut e = edit.borrow_mut();
            // The draft is not in history, so undoing past it would silently
            // drop the previous annotation while the text field stayed open.
            if undo {
                if e.text.take().is_none() {
                    e.session.undo();
                }
            } else if e.text.is_none() {
                e.session.redo();
            }
        }
        refresh_group(cx, group);
        cx.notify();
    }

    /// Edit-space point at this window's client origin.
    fn edit_offset(&self, window: &Window) -> Option<Point> {
        let edit = pin_edit::edit_for_group(self.group())?;
        let origin = edit.borrow().origin;
        let me = client_rect(window.win32_hwnd())?;
        Some(Point {
            x: (me.left - origin.0) as f64,
            y: (me.top - origin.1) as f64,
        })
    }

    fn edit_point(&self, window: &Window, pos: GpuiPoint<Pixels>) -> Option<Point> {
        let off = self.edit_offset(window)?;
        let scale = window.scale_factor().max(0.01) as f64;
        Some(Point {
            x: off.x + f64::from(f32::from(pos.x)) * scale,
            y: off.y + f64::from(f32::from(pos.y)) * scale,
        })
    }

    fn edit_press(&mut self, pt: Point, cx: &mut Context<Self>) {
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        {
            let e = &mut *edit.borrow_mut();
            e.color_open = false;
            annotate::commit_text_draft(&mut e.session, &mut e.text);
            match e.session.pointer_pressed(pt) {
                PressKind::RequestText => {
                    e.text = Some(TextDraft::new(pt));
                    self.edit_down = None;
                }
                PressKind::Capture => self.edit_down = Some(pt),
            }
        }
        refresh_group(cx, group);
        cx.notify();
    }

    fn edit_drag(&mut self, pt: Point, cx: &mut Context<Self>) {
        let Some(down) = self.edit_down else {
            return;
        };
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        {
            let mut e = edit.borrow_mut();
            if e.session.tool != Tool::Select && e.session.tool != Tool::Text {
                if !e.session.is_dragging() {
                    let _ = e.session.pointer_pressed(down);
                }
                e.session.pointer_moved(pt);
            } else if e.session.is_dragging() && e.session.selected_id.is_some() {
                e.session.pointer_moved(pt);
            }
        }
        refresh_group(cx, group);
        cx.notify();
    }

    fn edit_release(&mut self, cx: &mut Context<Self>) {
        if self.edit_down.take().is_none() {
            return;
        }
        let group = self.group();
        if let Some(edit) = pin_edit::edit_for_group(group) {
            edit.borrow_mut().session.pointer_released();
        }
        refresh_group(cx, group);
        cx.notify();
    }

    /// Only Backspace lives here. Characters arrive through the
    /// `EntityInputHandler` below (WM_CHAR and the IME result string).
    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "backspace" {
            return;
        }
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        let went = edit
            .borrow_mut()
            .text
            .as_mut()
            .is_some_and(|t| t.backspace());
        if went {
            refresh_group(cx, group);
            cx.notify();
        }
    }

    // ---- mouse ----------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        if self.editing() {
            if let Some(pt) = self.edit_point(window, event.position) {
                self.edit_press(pt, cx);
            }
            return;
        }
        if event.click_count >= 2 {
            self.close_group(window, cx);
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
        // Whoever was snapped against this pin loses its ✓: the seam is
        // about to move.
        clear_pendings_targeting(cx, &[self.id], self.id);
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        if self.editing() {
            if let Some(pt) = self.edit_point(window, event.position) {
                self.edit_drag(pt, cx);
            }
            return;
        }
        let Some((dx, dy)) = self.drag_offset else {
            return;
        };
        let hwnd = window.win32_hwnd();
        let mut cursor = POINT::default();
        let (Some(outer), Some(client)) = (outer_rect(hwnd), client_rect(hwnd)) else {
            return;
        };
        unsafe {
            if GetCursorPos(&mut cursor).is_err() {
                return;
            }
        }
        // Raw target from the grab point, expressed as a client rect.
        let raw = client.translated(cursor.x - dx - outer.left, cursor.y - dy - outer.top);

        let members = group_members(self.id);
        let had_pending = self.pending.is_some();
        let mut target = raw;
        self.pending = None;
        // Only a lone pin joins a shape; a whole group is just moved.
        if members.is_empty() && !event.modifiers.alt {
            let others: Vec<PinEntry> = entries().into_iter().filter(|e| e.id != self.id).collect();
            let tiles: Vec<(u64, Tile)> = others
                .iter()
                .filter_map(|e| {
                    let rect = client_rect(e.hwnd)?;
                    Some((
                        e.id,
                        Tile {
                            rect,
                            group: e.group,
                        },
                    ))
                })
                .collect();
            let rects: Vec<Tile> = tiles.iter().map(|(_, t)| *t).collect();
            let tolerance = if had_pending { SNAP_EXIT } else { SNAP_ENTER };
            if let Some(snap) = find_snap(raw, &rects, snap_rules(tolerance)) {
                target = snap.rect;
                self.pending = Some(Pending {
                    target: tiles[snap.target].0,
                    side: snap.side,
                });
            }
        }

        let mut hwnds = vec![hwnd];
        hwnds.extend(members.iter().map(|m| m.hwnd));
        move_windows(&hwnds, target.left - client.left, target.top - client.top);
        // The ✓ rides on the seam, so any snapped move needs a repaint.
        if had_pending || self.pending.is_some() {
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.button {
            MouseButton::Left => {
                if self.editing() {
                    self.edit_release(cx);
                } else if self.drag_offset.take().is_some() {
                    cx.notify();
                }
            }
            MouseButton::Right => self.open_menu(cx),
            _ => {}
        }
    }

    /// Right-click: a text draft is dropped (as in the overlay); otherwise the
    /// hidden window is asked to show the context menu. It runs the modal
    /// loop where no gpui window is mid-update, then calls back.
    fn open_menu(&mut self, cx: &mut Context<Self>) {
        let group = self.group();
        if let Some(edit) = pin_edit::edit_for_group(group) {
            if edit.borrow_mut().text.take().is_some() {
                refresh_group(cx, group);
                cx.notify();
                return;
            }
        }
        unsafe {
            let _ = PostMessageW(
                Some(hook_target()),
                WM_ZSHOT_PIN_MENU,
                WPARAM(self.id as usize),
                LPARAM(0),
            );
        }
    }

    /// Wheel zoom resizes the window with the image -- the frame hugs the
    /// pixels at every zoom level -- anchored on the cursor, so the image
    /// point under it stays put (the PixPin zoom feel). A group scales every
    /// tile about the same anchor, edge by edge, so seams stay closed.
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Marks are laid out in screen pixels for the duration of an edit.
        if self.editing() {
            return;
        }
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * 32.0,
        };
        if dy == 0.0 {
            return;
        }
        let factor = if dy > 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
        let hwnd = window.win32_hwnd();
        let scale = window.scale_factor().max(0.01);
        let Some(client) = client_rect(hwnd) else {
            return;
        };
        // Cursor in screen pixels; event.position is logical, client-relative.
        let anchor = (
            (client.left as f32 + f32::from(event.position.x) * scale).round() as i32,
            (client.top as f32 + f32::from(event.position.y) * scale).round() as i32,
        );
        self.zoom_group(factor, anchor, window, cx);
    }

    /// Scales this tile and its group about `anchor` (screen px) by `factor`,
    /// clamped to the zoom range.
    fn zoom_group(
        &mut self,
        factor: f32,
        anchor: (i32, i32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A thin strip must not zoom into a zero-pixel window: keep the
        // smaller side at a grabbable 24 physical px.
        let min_dim = self.pixel_size.0.min(self.pixel_size.1) as f32;
        let floor = MIN_ZOOM.max(24.0 / min_dim);
        let new_zoom = (self.zoom * factor).clamp(floor, MAX_ZOOM);
        if new_zoom == self.zoom {
            return;
        }
        let factor = new_zoom / self.zoom;
        self.apply_zoom(window.win32_hwnd(), anchor, factor);
        for m in group_members(self.id) {
            let _ = m.handle.update(cx, |view, window, cx| {
                view.apply_zoom(window.win32_hwnd(), anchor, factor);
                cx.notify();
            });
        }
        cx.notify();
    }

    fn apply_zoom(&mut self, hwnd: HWND, anchor: (i32, i32), factor: f32) {
        let Some(client) = client_rect(hwnd) else {
            return;
        };
        let new_zoom = self.zoom * factor;
        let rect = zoom_rect(
            self.precise_rect(client),
            anchor,
            new_zoom as f64 / self.zoom as f64,
        );
        self.zoom = new_zoom;
        self.layout_rect = Some(rect);
        set_client_rect(hwnd, rounded_rect(rect));
    }

    /// Back to 100%, the group anchored on its top-left corner.
    fn actual_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() || self.zoom == 1.0 {
            return;
        }
        let Some(me) = client_rect(window.win32_hwnd()) else {
            return;
        };
        let bbox = group_members(self.id)
            .iter()
            .filter_map(|m| client_rect(m.hwnd))
            .fold(me, |acc, r| acc.union(&r));
        let factor = 1.0 / self.zoom;
        self.zoom_group(factor, (bbox.left, bbox.top), window, cx);
    }

    // ---- export ---------------------------------------------------------------

    fn copy(&mut self, _: &CopyPin, window: &mut Window, cx: &mut Context<Self>) {
        self.export(window, cx, false, true);
    }

    fn save(&mut self, _: &SavePin, window: &mut Window, cx: &mut Context<Self>) {
        self.export(window, cx, true, false);
    }

    /// Composites this pin's group at its screen layout and hands the result
    /// to the regular export path. Compositing runs off the UI thread.
    fn export(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        write_file: bool,
        copy: bool,
    ) {
        if self.editing() {
            self.finish_edit(true, window, cx);
        }
        let tiles = snapshots(cx, self.group(), self.snapshot(window));
        std::thread::spawn(move || {
            let images: Vec<TileImage> = tiles
                .iter()
                .map(|t| TileImage {
                    rect: t.precise_rect,
                    pixels: t.pixels.as_slice(),
                    width: t.size.0,
                    height: t.size.1,
                    zoom: t.zoom,
                })
                .collect();
            let Some((mut px, w, h)) = compose(&images) else {
                crate::perf::log_app("pin export: group too large to compose");
                return;
            };
            if copy {
                flatten_gaps(&mut px, [255, 255, 255, 255]);
            }
            crate::app::export_pin_pixels(px, w, h, write_file, copy);
        });
    }

    // ---- chrome ---------------------------------------------------------------

    /// A group shows one ✕, on the tile whose top-right corner is nearest the
    /// group's top-right corner. A lone pin is trivially that tile.
    fn holds_close_button(&self, window: &Window) -> bool {
        let members = group_members(self.id);
        if members.is_empty() {
            return true;
        }
        let Some(me) = client_rect(window.win32_hwnd()) else {
            return true;
        };
        let rects: Vec<(u64, IRect)> = std::iter::once((self.id, me))
            .chain(
                members
                    .iter()
                    .filter_map(|m| Some((m.id, client_rect(m.hwnd)?))),
            )
            .collect();
        let bbox = rects.iter().skip(1).fold(me, |acc, (_, r)| acc.union(r));
        let corner_gap = |r: &IRect| (bbox.right - r.right) as i64 + (r.top - bbox.top) as i64;
        rects
            .iter()
            .min_by_key(|(id, r)| (corner_gap(r), *id))
            .is_some_and(|(id, _)| *id == self.id)
    }

    /// Where the ✓ sits, in logical client coordinates: centred on the seam
    /// with the target, just inside this window.
    fn stitch_button_origin(&self, window: &Window) -> Option<(f32, f32)> {
        let p = self.pending?;
        let me = client_rect(window.win32_hwnd())?;
        let other = client_rect(entry(p.target)?.hwnd)?;
        let scale = window.scale_factor().max(0.01);
        let (w, h) = (me.width() as f32 / scale, me.height() as f32 / scale);
        let mid_y = (me.top.max(other.top) + me.bottom.min(other.bottom)) as f32 / 2.0;
        let mid_x = (me.left.max(other.left) + me.right.min(other.right)) as f32 / 2.0;
        let along_y = ((mid_y - me.top as f32) / scale - 11.0).clamp(6.0, (h - 28.0).max(6.0));
        let along_x = ((mid_x - me.left as f32) / scale - 11.0).clamp(6.0, (w - 28.0).max(6.0));
        Some(match p.side {
            // Attached to the target's right: the seam is this window's left edge.
            Side::Right => (6.0, along_y),
            Side::Left => (w - 28.0, along_y),
            Side::Bottom => (along_x, 6.0),
            Side::Top => (along_x, h - 28.0),
        })
    }
}

/// Repaints other pins whose chrome depends on group membership.
fn refresh(cx: &mut App, pins: &[PinEntry]) {
    for e in pins {
        let _ = e.handle.update(cx, |_, _, cx| cx.notify());
    }
}

/// Drops every other pin's pending snap that points at one of `ids`. `except`
/// is the caller's own window, which cannot be re-entered.
fn clear_pendings_targeting(cx: &mut App, ids: &[u64], except: u64) {
    for e in entries() {
        if e.id == except {
            continue;
        }
        let _ = e.handle.update(cx, |view, _, cx| {
            if view.pending.is_some_and(|p| ids.contains(&p.target)) {
                view.pending = None;
                cx.notify();
            }
        });
    }
}

impl Focusable for PinView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// IME plumbing for the group's text draft; see `annotate::TextDraft`.
impl EntityInputHandler for PinView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let edit = pin_edit::edit_for_group(self.group())?;
        let e = edit.borrow();
        Some(e.text.as_ref()?.text_for_range(range_utf16, actual_range))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let edit = pin_edit::edit_for_group(self.group())?;
        let e = edit.borrow();
        Some(e.text.as_ref()?.selected_range())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let edit = pin_edit::edit_for_group(self.group())?;
        let e = edit.borrow();
        e.text.as_ref()?.marked_range()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if let Some(edit) = pin_edit::edit_for_group(self.group()) {
            if let Some(t) = edit.borrow_mut().text.as_mut() {
                t.unmark();
            }
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        if let Some(t) = edit.borrow_mut().text.as_mut() {
            t.replace(range_utf16, new_text);
        }
        refresh_group(cx, group);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = self.group();
        let Some(edit) = pin_edit::edit_for_group(group) else {
            return;
        };
        if let Some(t) = edit.borrow_mut().text.as_mut() {
            t.replace_and_mark(range_utf16, new_text);
        }
        refresh_group(cx, group);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let offset = self.edit_offset(window)?;
        let edit = pin_edit::edit_for_group(self.group())?;
        let e = edit.borrow();
        let scale = window.scale_factor().max(0.01);
        Some(e.text.as_ref()?.ime_bounds(element_bounds, offset, scale))
    }

    fn character_index_for_point(
        &mut self,
        _: GpuiPoint<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for PinView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The window always hugs the image, so drawing at the viewport size
        // keeps a group's tiles pixel-exact after edge-wise zooming.
        let viewport = window.viewport_size();
        let w = f32::from(viewport.width);
        let h = f32::from(viewport.height);
        let scale = window.scale_factor().max(0.01);
        let editing = pin_edit::edit_for_group(self.group());
        let buttons = editing.is_none() && w >= BUTTON_MIN_SIDE && h >= BUTTON_MIN_SIDE;
        let close_here = buttons && self.holds_close_button(window);
        let stitch_at = buttons.then(|| self.stitch_button_origin(window)).flatten();

        // Everything the edit layers need, gathered while the group's edit is
        // borrowed; the paint closure runs later and must own its data.
        let mut cursor = CursorStyle::Arrow;
        let mut edit_paint = None;
        let mut text_layer = None;
        if let (Some(edit), Some(offset)) = (editing.as_ref(), self.edit_offset(window)) {
            let e = edit.borrow();
            let marks: Vec<Element> = e.session.visible().cloned().collect();
            let masks = annotate::mark_masks(&e.mask_cache, &marks);
            let mosaic = annotate::mosaic_tiles(&e.mosaic_cache, &marks, e.extent, |x, y, w, h| {
                e.readback(x, y, w, h)
            });
            text_layer = Some(
                annotate::text_layer(TextLayer {
                    frame: Rect::new(0.0, 0.0, w as f64, h as f64),
                    offset,
                    scale,
                    elements: &e.session.elements,
                    draft: e.text.as_ref(),
                    stroke: e.session.stroke,
                })
                .into_any_element(),
            );
            let input = e
                .text
                .is_some()
                .then(|| (self.focus_handle.clone(), cx.entity()));
            cursor = match (e.text.is_some(), e.session.tool) {
                (true, _) | (_, Tool::Text) => CursorStyle::IBeam,
                (_, Tool::Select) => CursorStyle::Arrow,
                _ => CursorStyle::Crosshair,
            };
            edit_paint = Some((marks, masks, mosaic, offset, input));
        }
        // Drags and strokes must keep flowing once the cursor leaves this
        // window (mouse capture delivers them), but element listeners are
        // hover-gated: move and left-up are taken at window level instead.
        let on_move = cx.listener(Self::on_mouse_move);
        let on_up = cx.listener(Self::on_mouse_up);
        let mouse_layer = canvas(
            move |_, _, _| (),
            move |bounds, _, window, cx| {
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_move(e, window, cx);
                    }
                });
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                        on_up(e, window, cx);
                    }
                });
                if let Some((marks, masks, mosaic, offset, input)) = edit_paint {
                    if let Some((focus, entity)) = input {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
                    }
                    annotate::paint_marks(window, &marks, &masks, &mosaic, offset, scale);
                }
            },
        )
        .absolute()
        .size_full();

        div()
            .id("pin-root")
            .size_full()
            .overflow_hidden()
            .key_context("Pin")
            .track_focus(&self.focus_handle)
            .cursor(cursor)
            .occlude()
            .rounded_md()
            .border_1()
            .border_color(hsla(0., 0., 1., 0.7))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::detach))
            .on_action(cx.listener(Self::annotate))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .child(
                img(ImageSource::Render(self.image.clone()))
                    .w(px(w))
                    .h(px(h)),
            )
            .child(
                div()
                    .absolute()
                    .size_full()
                    .child(mouse_layer)
                    .children(text_layer),
            )
            // A close affordance that does not cover small pins: under 48
            // logical px on either side, Esc / double-click remain the way out.
            .children(close_here.then(|| {
                round_button("pin-close", 6.0, "✕", hsla(0.02, 0.7, 0.5, 0.9))
                    .right(px(6.))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_group(window, cx);
                        }),
                    )
            }))
            .children(stitch_at.map(|(x, y)| {
                round_button("pin-stitch", y, "✓", hsla(0.38, 0.6, 0.4, 0.95))
                    .left(px(x))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.confirm_stitch(window, cx);
                        }),
                    )
            }))
    }
}

/// A 22 px round overlay button. The caller sets the horizontal anchor
/// (`left` or `right`) and the mouse-up handler; the press is swallowed here
/// so it neither starts a drag nor reads as a double-click close.
fn round_button(
    id: &'static str,
    y: f32,
    label: &'static str,
    hover_bg: gpui::Hsla,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .top(px(y))
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
        .hover(move |s| s.bg(hover_bg))
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .child(label)
}

// ---- context menu -------------------------------------------------------------

const CMD_COPY: u32 = 1;
const CMD_SAVE: u32 = 2;
const CMD_ANNOTATE: u32 = 3;
const CMD_FINISH_EDIT: u32 = 4;
const CMD_CANCEL_EDIT: u32 = 5;
const CMD_STITCH: u32 = 6;
const CMD_DETACH: u32 = 7;
const CMD_ACTUAL_SIZE: u32 = 8;
const CMD_CLOSE: u32 = 9;
const CMD_CLOSE_ALL: u32 = 10;

pub(crate) enum MenuEntry {
    Item { cmd: u32, label: &'static str },
    Separator,
}

/// The menu for pin `id`, built from its current state. `None` when the pin
/// is gone.
pub(crate) fn menu_items(cx: &mut App, id: u64) -> Option<Vec<MenuEntry>> {
    let e = entry(id)?;
    let (pending, zoomed) = e
        .handle
        .read(cx)
        .map(|v| (v.pending.is_some(), v.zoom != 1.0))
        .ok()?;
    let editing = pin_edit::is_editing(e.group);
    let grouped = group_entries(e.group).len() > 1;
    use MenuEntry::*;
    let mut items = vec![
        Item {
            cmd: CMD_COPY,
            label: "复制\tCtrl+C",
        },
        Item {
            cmd: CMD_SAVE,
            label: "保存\tCtrl+S",
        },
        Separator,
    ];
    if editing {
        items.push(Item {
            cmd: CMD_FINISH_EDIT,
            label: "完成标注\tEnter",
        });
        items.push(Item {
            cmd: CMD_CANCEL_EDIT,
            label: "取消标注\tEsc",
        });
    } else {
        items.push(Item {
            cmd: CMD_ANNOTATE,
            label: "标注\tCtrl+E",
        });
        if pending {
            items.push(Item {
                cmd: CMD_STITCH,
                label: "确认拼接\tEnter",
            });
        }
        if grouped {
            items.push(Item {
                cmd: CMD_DETACH,
                label: "从组中拆开\tCtrl+D",
            });
        }
        if zoomed {
            items.push(Item {
                cmd: CMD_ACTUAL_SIZE,
                label: "实际大小",
            });
        }
    }
    items.push(Separator);
    items.push(Item {
        cmd: CMD_CLOSE,
        label: "关闭\tEsc",
    });
    items.push(Item {
        cmd: CMD_CLOSE_ALL,
        label: "关闭全部钉图",
    });
    Some(items)
}

/// Shows the native popup at the cursor, owned by `owner` (the hidden
/// window, as for the tray). Blocks in the menu's modal loop.
pub(crate) fn track_menu(owner: HWND, items: &[MenuEntry]) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, item) in items.iter().enumerate() {
            match item {
                MenuEntry::Separator => {
                    let _ = InsertMenuW(
                        menu,
                        i as u32,
                        MF_BYPOSITION | MF_SEPARATOR,
                        0,
                        PCWSTR::null(),
                    );
                }
                MenuEntry::Item { cmd, label } => {
                    // The menu copies the string on insert.
                    let text: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                    let _ = InsertMenuW(
                        menu,
                        i as u32,
                        MF_BYPOSITION | MF_STRING,
                        *cmd as usize,
                        PCWSTR(text.as_ptr()),
                    );
                }
            }
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            owner,
            None,
        );
        let _ = DestroyMenu(menu);
        let cmd = cmd.0 as u32;
        (cmd != 0).then_some(cmd)
    }
}

pub(crate) fn run_menu_command(cx: &mut App, id: u64, cmd: u32) {
    if cmd == CMD_CLOSE_ALL {
        for e in entries() {
            if pin_edit::is_editing(e.group) {
                pin_edit::finish(cx, e.group, false, Vec::new());
            }
            let _ = e.handle.update(cx, |_, window, _| window.remove_window());
        }
        PINS.with(|slot| slot.borrow_mut().clear());
        return;
    }
    let Some(handle) = entry_handle(id) else {
        return;
    };
    let _ = handle.update(cx, |view, window, cx| {
        // The menu took the foreground; keys should reach the pin again.
        window.focus(&view.focus_handle);
        match cmd {
            CMD_COPY => view.export(window, cx, false, true),
            CMD_SAVE => view.export(window, cx, true, false),
            CMD_ANNOTATE => view.start_edit(window, cx),
            CMD_FINISH_EDIT => view.finish_edit(true, window, cx),
            CMD_CANCEL_EDIT => view.finish_edit(false, window, cx),
            CMD_STITCH => view.confirm_stitch(window, cx),
            CMD_DETACH => view.detach(&DetachPin, window, cx),
            CMD_ACTUAL_SIZE => view.actual_size(window, cx),
            CMD_CLOSE => view.close_group(window, cx),
            _ => {}
        }
    });
}

// ---- pixels / geometry ----------------------------------------------------------

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
    // `out` is tightly packed BGRA, so the remainder is always empty.
    for px in out.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    out
}

/// Outer HWND rect whose client area equals `desired` (screen coordinates),
/// mirroring the overlay's helper. A caption-free window still reports small
/// non-client insets on some themes; without this the pinned image shifts off
/// the captured pixels.
pub(crate) fn window_rect_for_client_area(hwnd: HWND, desired: IRect) -> IRect {
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
    IRect::new(
        desired.left - left,
        desired.top - top,
        desired.right + right,
        desired.bottom + bottom,
    )
}

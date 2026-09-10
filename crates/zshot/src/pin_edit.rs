//! Annotating a pinned image, or a whole stitched group at once. The group
//! shares one `Session`: every tile paints the same element list offset by
//! its own position, the tile that receives the press drives the stroke
//! (mouse capture keeps the events flowing when the cursor crosses a seam),
//! and 完成 composites the group once, rasterizes the marks, and hands each
//! tile its slice back. A separate non-activating toolbar window sits under
//! the group while the edit is open.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, FocusHandle, Focusable, Point as GpuiPoint,
    Render, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, WS_EX_NOACTIVATE,
};

use crate::geom::Rect;
use crate::overlay::annotate::{
    self, MaskCache, MosaicCache, SwatchesModel, TextDraft, ToolbarModel,
};
use crate::overlay::composite::apply_annotations;
use crate::overlay::editor::{Element, Session};
use crate::overlay::{theme, toolbar};
use crate::pin::{self, TileSnapshot};
use crate::pin_snap::{compose, crop, layout, tile_dest, IRect, TileImage};

/// The pin toolbar: the overlay's drawing tools without export, OCR or
/// translation. Hidden items from settings apply here too.
const ITEMS: &[&str] = &[
    toolbar::SELECT,
    toolbar::RECT,
    toolbar::ARROW,
    toolbar::PEN,
    toolbar::MOSAIC,
    toolbar::TEXT,
    toolbar::COLOR,
    toolbar::UNDO,
    toolbar::REDO,
    toolbar::CLEAR,
    toolbar::COMPLETE,
    toolbar::CANCEL,
];

/// One open annotation session over a group.
pub struct GroupEdit {
    pub session: Session,
    pub text: Option<TextDraft>,
    pub color_open: bool,
    /// Edit space: screen physical pixels relative to this origin, the
    /// group's bounding box top-left when the edit began. Tiles cannot move
    /// or zoom while editing, so it stays valid.
    pub origin: (i32, i32),
    pub extent: (u32, u32),
    /// The group composited in screen pixels, for mosaic averaging.
    backdrop: Vec<u8>,
    pub mask_cache: MaskCache,
    pub mosaic_cache: MosaicCache,
    toolbar: Option<WindowHandle<PinToolbar>>,
}

impl GroupEdit {
    fn commit_drafts(&mut self) {
        self.session.pointer_released();
        annotate::commit_text_draft(&mut self.session, &mut self.text);
    }

    pub fn toolbar_handle(&self) -> Option<WindowHandle<PinToolbar>> {
        self.toolbar
    }

    /// BGRA pixels of an edit-space rect, tight stride.
    pub fn readback(&self, x: u32, y: u32, w: u32, h: u32) -> Option<(Vec<u8>, u32, u32, u32)> {
        let rect = IRect::from_origin_size(x as i32, y as i32, w as i32, h as i32);
        let px = crop(&self.backdrop, self.extent.0, self.extent.1, rect)?;
        Some((px, w, h, w * 4))
    }
}

thread_local! {
    static EDITS: RefCell<HashMap<u64, Rc<RefCell<GroupEdit>>>> = RefCell::new(HashMap::new());
}

pub fn edit_for_group(group: u64) -> Option<Rc<RefCell<GroupEdit>>> {
    EDITS.with(|slot| slot.borrow().get(&group).cloned())
}

pub fn is_editing(group: u64) -> bool {
    EDITS.with(|slot| slot.borrow().contains_key(&group))
}

/// Opens an edit over `group`, whose current tiles are `tiles`.
pub(crate) fn start(cx: &mut App, group: u64, tiles: &[TileSnapshot]) {
    if is_editing(group) || tiles.is_empty() {
        return;
    }
    let bbox = tiles
        .iter()
        .skip(1)
        .fold(tiles[0].rect, |acc, t| acc.union(&t.rect));
    // Zoom 1 for every tile forces a screen-pixel canvas: the backdrop is
    // what the user sees, resampled tiles included.
    let images: Vec<TileImage> = tiles
        .iter()
        .map(|t| TileImage {
            rect: t.rect.precise(),
            pixels: t.pixels.as_slice(),
            width: t.size.0,
            height: t.size.1,
            zoom: 1.0,
        })
        .collect();
    let Some((backdrop, bw, bh)) = compose(&images) else {
        crate::perf::log_app("pin edit: group too large to annotate");
        return;
    };
    let edit = Rc::new(RefCell::new(GroupEdit {
        session: Session::new(),
        text: None,
        color_open: false,
        origin: (bbox.left, bbox.top),
        extent: (bw, bh),
        backdrop,
        mask_cache: RefCell::new(None),
        mosaic_cache: RefCell::new(None),
        toolbar: None,
    }));
    EDITS.with(|slot| slot.borrow_mut().insert(group, edit.clone()));
    let toolbar = open_toolbar(cx, group, bbox);
    edit.borrow_mut().toolbar = toolbar;
    pin::refresh_group(cx, group);
}

/// Ends the edit. With `commit`, the marks are rasterized onto the tiles:
/// the group is composited once at its native resolution, annotated, and
/// sliced back, so seams and mosaic cells match the preview exactly. Tiles
/// are updated in place; the one the caller is currently inside (which
/// cannot be re-entered) comes back as `(id, pixels, w, h, zoom)` for the
/// caller to apply itself.
pub(crate) fn finish(
    cx: &mut App,
    group: u64,
    commit: bool,
    tiles: Vec<TileSnapshot>,
) -> Vec<(u64, Vec<u8>, u32, u32, f32)> {
    let Some(edit) = EDITS.with(|slot| slot.borrow_mut().remove(&group)) else {
        return Vec::new();
    };
    let mut leftover = Vec::new();
    {
        let mut e = edit.borrow_mut();
        if let Some(toolbar) = e.toolbar.take() {
            // Fails harmlessly when the toolbar itself is the caller; it
            // removes its own window after this returns.
            let _ = toolbar.update(cx, |_, window, _| window.remove_window());
        }
        if commit {
            let e = &mut *e;
            // A stroke whose release never arrived is still a draft; land it
            // rather than lose it.
            e.commit_drafts();
            if !e.session.elements.is_empty() {
                for (id, px, w, h, zoom) in flatten(&e.session.elements, &tiles) {
                    let applied = pin::entry_handle(id).and_then(|handle| {
                        handle
                            .update(cx, |view, _, cx| {
                                view.replace_pixels(px.clone(), w, h, zoom);
                                cx.notify();
                            })
                            .ok()
                    });
                    if applied.is_none() {
                        leftover.push((id, px, w, h, zoom));
                    }
                }
            }
        }
    }
    pin::refresh_group(cx, group);
    leftover
}

/// Rasterizes `elements` (edit space) over the tiles and returns each tile's
/// new native pixels.
fn flatten(elements: &[Element], tiles: &[TileSnapshot]) -> Vec<(u64, Vec<u8>, u32, u32, f32)> {
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
    let Some(lay) = layout(
        &tiles
            .iter()
            .map(|t| (t.precise_rect, t.zoom))
            .collect::<Vec<_>>(),
    ) else {
        return Vec::new();
    };
    let Some((mut canvas, cw, ch)) = compose(&images) else {
        crate::perf::log_app("pin edit: group too large to flatten");
        return Vec::new();
    };
    // Elements are screen pixels relative to the bounding box; the canvas is
    // that space scaled by k about the same corner.
    let scaled: Vec<Element> = elements.iter().map(|el| el.scaled(lay.k as f64)).collect();
    apply_annotations(&mut canvas, cw, ch, cw * 4, 0.0, 0.0, &scaled);
    tiles
        .iter()
        .filter_map(|t| {
            let dest = tile_dest(t.precise_rect, &lay);
            let px = crop(&canvas, cw, ch, dest)?;
            let zoom = 1.0 / lay.k;
            Some((t.id, px, dest.width() as u32, dest.height() as u32, zoom))
        })
        .collect()
}

// ---- toolbar window ---------------------------------------------------------

/// The floating tool bar under an editing group. Its window never activates,
/// so keyboard focus (and the IME) stays with the tile being drawn on.
pub struct PinToolbar {
    group: u64,
    focus_handle: FocusHandle,
}

fn visible_items() -> Vec<&'static str> {
    let hidden = crate::config::get().screenshot.hidden_toolbar_items;
    ITEMS
        .iter()
        .copied()
        .filter(|id| toolbar::is_visible(id, &hidden))
        .collect()
}

/// Logical layout of the toolbar window: the swatch row above, a gap, then
/// the bar. Returns (window w, window h, bar x, bar y, bar w).
fn toolbar_layout(n: usize) -> (f64, f64, f64, f64, f64) {
    let (bw, bh) = toolbar::bar_size(n);
    let (sw, sh) = toolbar::swatches_size(theme::SWATCHES.len());
    let w = bw.max(sw);
    let bar_y = sh + toolbar::ANCHOR_GAP;
    (w, bar_y + bh, (w - bw) / 2.0, bar_y, bw)
}

fn open_toolbar(cx: &mut App, group: u64, bbox: IRect) -> Option<WindowHandle<PinToolbar>> {
    let n = visible_items().len();
    let (w, h, _, _, _) = toolbar_layout(n);
    let bounds = Bounds {
        origin: GpuiPoint::new(px(bbox.left as f32), px(bbox.bottom as f32)),
        size: size(px(w as f32), px(h as f32)),
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                topmost: true,
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| PinToolbar {
                    group,
                    focus_handle: cx.focus_handle(),
                })
            },
        )
        .ok()?;
    let _ = handle.update(cx, |_, window, _| {
        let hwnd = window.win32_hwnd();
        let scale = window.scale_factor().max(0.01) as f64;
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        }
        let sel = Rect::new(
            bbox.left as f64,
            bbox.top as f64,
            bbox.width() as f64,
            bbox.height() as f64,
        );
        let pos = toolbar::place_toolbar(
            sel,
            w * scale,
            h * scale,
            monitor_rect_for(bbox),
            toolbar::ANCHOR_GAP * scale,
        );
        place_window(
            hwnd,
            IRect::from_origin_size(
                pos.x.round() as i32,
                pos.y.round() as i32,
                (w * scale).round() as i32,
                (h * scale).round() as i32,
            ),
        );
    });
    Some(handle)
}

/// The monitor holding the group's centre, physical px; the group's own box
/// when enumeration fails.
fn monitor_rect_for(bbox: IRect) -> Rect {
    let cx = (bbox.left + bbox.right) / 2;
    let cy = (bbox.top + bbox.bottom) / 2;
    let mons = crate::capture::enumerate_monitors().unwrap_or_default();
    mons.iter()
        .find(|m| cx >= m.x && cx < m.x + m.w && cy >= m.y && cy < m.y + m.h)
        .or(mons.first())
        .map(|m| Rect::new(m.x as f64, m.y as f64, m.w as f64, m.h as f64))
        .unwrap_or_else(|| {
            Rect::new(
                bbox.left as f64,
                bbox.top as f64,
                bbox.width() as f64,
                bbox.height() as f64,
            )
        })
}

fn place_window(hwnd: HWND, client: IRect) {
    let outer = pin::window_rect_for_client_area(hwnd, client);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            outer.left,
            outer.top,
            outer.width(),
            outer.height(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

impl PinToolbar {
    fn on_item(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = edit_for_group(self.group) else {
            window.remove_window();
            return;
        };
        let handled = {
            let e = &mut *edit.borrow_mut();
            annotate::handle_common_toolbar(&mut e.session, &mut e.text, &mut e.color_open, id)
        };
        if handled {
            pin::refresh_group(cx, self.group);
            cx.notify();
            return;
        }
        match id {
            toolbar::COMPLETE | toolbar::CANCEL => {
                let tiles = pin::snapshots(cx, self.group, None);
                finish(cx, self.group, id == toolbar::COMPLETE, tiles);
                window.remove_window();
            }
            _ => {}
        }
    }
}

impl Focusable for PinToolbar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PinToolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(edit) = edit_for_group(self.group) else {
            return div().size_full();
        };
        let items = visible_items();
        let (w, _, bar_x, bar_y, bar_w) = toolbar_layout(items.len());
        let (model, swatches) = {
            let e = edit.borrow();
            let model = ToolbarModel {
                origin: (bar_x as f32, bar_y as f32),
                items: items.clone(),
                tool_id: e.session.tool.id(),
                can_undo: e.session.history.can_undo(),
                can_redo: e.session.history.can_redo(),
                color_open: e.color_open,
                stroke: e.session.stroke,
            };
            let swatches = e.color_open.then(|| {
                let (sw, _) = toolbar::swatches_size(theme::SWATCHES.len());
                let idx = items
                    .iter()
                    .position(|id| *id == toolbar::COLOR)
                    .unwrap_or(0);
                let bar = Rect::new(bar_x, bar_y, bar_w, toolbar::H);
                let x = (toolbar::item_center_x(bar, idx) - sw / 2.0).clamp(0.0, (w - sw).max(0.0));
                SwatchesModel {
                    origin: (x as f32, 0.0),
                    current: e.session.stroke,
                }
            });
            (model, swatches)
        };
        let group = self.group;
        div()
            .size_full()
            .child(annotate::toolbar_element(
                model,
                Rc::new(|id, this: &mut Self, window, cx| this.on_item(id, window, cx)),
                cx,
            ))
            .children(swatches.map(|m| {
                annotate::swatches_element(
                    m,
                    Rc::new(move |argb, _: &mut Self, _, cx| {
                        if let Some(edit) = edit_for_group(group) {
                            let mut e = edit.borrow_mut();
                            e.session.stroke = argb;
                            e.color_open = false;
                        }
                        cx.notify();
                    }),
                    cx,
                )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect as GRect;
    use crate::overlay::editor::Shape;
    use std::sync::Arc;

    fn tile(id: u64, x: i32, w: u32, h: u32, zoom: f32) -> TileSnapshot {
        let screen_w = (w as f32 * zoom).round() as i32;
        let screen_h = (h as f32 * zoom).round() as i32;
        TileSnapshot {
            id,
            rect: IRect::from_origin_size(x, 0, screen_w, screen_h),
            precise_rect: GRect::new(
                x as f64,
                0.0,
                w as f64 * zoom as f64,
                h as f64 * zoom as f64,
            ),
            pixels: Arc::new(vec![255u8; (w * h * 4) as usize]),
            size: (w, h),
            zoom,
        }
    }

    fn marked(px: &[u8]) -> usize {
        px.chunks_exact(4)
            .filter(|p| p[..3] != [255, 255, 255])
            .count()
    }

    #[test]
    fn flatten_lands_one_rect_on_both_tiles_of_a_seam() {
        let a = tile(1, 0, 20, 20, 1.0);
        let b = tile(2, 20, 20, 20, 1.0);
        // Edit space is screen px from the group's top-left: a rect from x=4
        // to x=36 straddles the seam at x=20.
        let el = Element {
            id: 1,
            bounds: GRect::new(4.0, 4.0, 32.0, 12.0),
            stroke: 0xFFFF_0000,
            stroke_width: 2.0,
            shape: Shape::Rect,
        };
        let out = flatten(&[el], &[a, b]);
        assert_eq!(out.len(), 2);
        for (id, px, w, h, zoom) in &out {
            assert_eq!((*w, *h), (20, 20), "tile {id} keeps its native size");
            assert_eq!(*zoom, 1.0);
            assert!(marked(px) > 0, "tile {id} received no stroke pixels");
        }
    }

    #[test]
    fn flatten_keeps_native_resolution_for_a_uniformly_zoomed_group() {
        let a = tile(1, 0, 10, 10, 2.0);
        let b = tile(2, 20, 10, 10, 2.0);
        let el = Element {
            id: 1,
            bounds: GRect::new(2.0, 2.0, 36.0, 16.0),
            stroke: 0xFFFF_0000,
            stroke_width: 2.0,
            shape: Shape::Rect,
        };
        let out = flatten(&[el], &[a, b]);
        for (_, px, w, h, zoom) in &out {
            assert_eq!((*w, *h), (10, 10));
            assert!((*zoom - 2.0).abs() < 1e-3);
            assert!(marked(px) > 0);
        }
    }

    #[test]
    fn export_commit_includes_unreleased_stroke_and_text_draft() {
        let mut edit = GroupEdit {
            session: Session::new(),
            text: None,
            color_open: false,
            origin: (0, 0),
            extent: (100, 100),
            backdrop: Vec::new(),
            mask_cache: RefCell::new(None),
            mosaic_cache: RefCell::new(None),
            toolbar: None,
        };
        edit.session.tool = crate::overlay::editor::Tool::Rect;
        edit.session
            .pointer_pressed(crate::geom::Point { x: 4.0, y: 4.0 });
        edit.session
            .pointer_moved(crate::geom::Point { x: 40.0, y: 40.0 });
        let mut text = TextDraft::new(crate::geom::Point { x: 5.0, y: 50.0 });
        text.buf = "export".into();
        edit.text = Some(text);
        edit.commit_drafts();
        assert!(edit.text.is_none());
        assert_eq!(edit.session.elements.len(), 2);
        assert!(edit
            .session
            .elements
            .iter()
            .any(|el| matches!(&el.shape, Shape::Text { text, .. } if text == "export")));
        let out = flatten(&edit.session.elements, &[tile(1, 0, 100, 100, 1.0)]);
        assert_eq!(out.len(), 1);
        assert!(marked(&out[0].1) > 0);
    }

    #[test]
    fn flatten_after_repeated_zoom_keeps_original_size() {
        let mut t = tile(1, 0, 100, 100, 1.0);
        for _ in 0..14 {
            let next = t.zoom * (1.0f32 / 1.1);
            t.precise_rect =
                crate::pin_snap::zoom_rect(t.precise_rect, (0, 0), next as f64 / t.zoom as f64);
            t.rect = crate::pin_snap::rounded_rect(t.precise_rect);
            t.zoom = next;
        }
        let el = Element {
            id: 1,
            bounds: GRect::new(2.0, 2.0, 10.0, 10.0),
            stroke: 0xFFFF_0000,
            stroke_width: 2.0,
            shape: Shape::Rect,
        };
        let out = flatten(&[el], &[t]);
        assert_eq!((out[0].2, out[0].3), (100, 100));
        assert!(marked(&out[0].1) > 0);
    }
}

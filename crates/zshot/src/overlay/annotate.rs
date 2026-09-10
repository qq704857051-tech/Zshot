//! Annotation UI shared by the capture overlay and pinned images: the text
//! draft the IME types into, the cached coverage masks and mosaic tiles behind
//! the live preview, the mark painter, and the toolbar / colour popup / text
//! layer elements. The hosting view owns the `Session` and decides what each
//! toolbar button does; this module renders and dispatches.
//!
//! Coordinates: elements live in the host's *edit space* (capture pixels for
//! the overlay, group pixels for a pin). Painters take an `offset`, the edit
//! space point that sits at the window's client origin, so one element list
//! can be drawn by several windows each showing a different part of it.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    div, fill, point, prelude::*, px, rgb, size, svg, AnyView, Bounds, BoxShadow, Context,
    CursorStyle, MouseButton, Pixels, Render, SharedString, UTF16Selection, Window,
};

use crate::geom::{Point, Rect};
use crate::overlay::composite::{element_mask, CoverageRun};
use crate::overlay::editor::{Element, Session, Shape, Tool, TEXT_FONT_SIZE};
use crate::overlay::{text_raster, theme, toolbar};

// ---- text draft -----------------------------------------------------------

/// A text annotation being typed. The caret is always at the end, so the IME
/// plumbing below is the minimum the platform needs: append committed text,
/// keep the composition string marked so the next update replaces it.
pub struct TextDraft {
    /// Edit-space origin of the text.
    pub pt: Point,
    pub buf: String,
    /// Byte range of the IME composition (未上屏) text inside `buf`; the next
    /// composition update or result replaces exactly this range.
    pub marked: Option<Range<usize>>,
}

impl TextDraft {
    pub fn new(pt: Point) -> Self {
        Self {
            pt,
            buf: String::new(),
            marked: None,
        }
    }

    pub fn text_for_range(
        &self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
    ) -> String {
        let buf = &self.buf;
        let range = utf16_to_byte(buf, range_utf16.start)..utf16_to_byte(buf, range_utf16.end);
        actual_range.replace(byte_to_utf16(buf, range.start)..byte_to_utf16(buf, range.end));
        buf[range].to_string()
    }

    pub fn selected_range(&self) -> UTF16Selection {
        let end = byte_to_utf16(&self.buf, self.buf.len());
        UTF16Selection {
            range: end..end,
            reversed: false,
        }
    }

    pub fn marked_range(&self) -> Option<Range<usize>> {
        let m = self.marked.as_ref()?;
        Some(byte_to_utf16(&self.buf, m.start)..byte_to_utf16(&self.buf, m.end))
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    fn resolve(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|r| utf16_to_byte(&self.buf, r.start)..utf16_to_byte(&self.buf, r.end))
            .or_else(|| self.marked.clone())
            .unwrap_or(self.buf.len()..self.buf.len())
    }

    pub fn replace(&mut self, range_utf16: Option<Range<usize>>, new_text: &str) {
        let range = self.resolve(range_utf16);
        self.buf.replace_range(range, new_text);
        self.marked = None;
    }

    pub fn replace_and_mark(&mut self, range_utf16: Option<Range<usize>>, new_text: &str) {
        let range = self.resolve(range_utf16);
        self.buf.replace_range(range.clone(), new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
    }

    /// Mid-composition the IME owns Backspace; returns whether a char went.
    pub fn backspace(&mut self) -> bool {
        if self.marked.is_some() {
            return false;
        }
        self.buf.pop().is_some()
    }

    /// Where the IME parks its candidate window: the whole draft line, not the
    /// exact caret x. `offset` is the edit-space point at the element origin.
    pub fn ime_bounds(
        &self,
        element_bounds: Bounds<Pixels>,
        offset: Point,
        scale: f32,
    ) -> Bounds<Pixels> {
        Bounds {
            origin: point(
                element_bounds.origin.x + px((self.pt.x - offset.x) as f32 / scale),
                element_bounds.origin.y + px((self.pt.y - offset.y) as f32 / scale),
            ),
            size: size(px(1.), px(TEXT_FONT_SIZE as f32 / scale)),
        }
    }
}

/// UTF-16 offset (what the platform IME speaks) to a byte offset in `s`.
fn utf16_to_byte(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, ch) in s.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += ch.len_utf16();
    }
    s.len()
}

fn byte_to_utf16(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].encode_utf16().count()
}

// ---- shared toolbar semantics ----------------------------------------------

/// The buttons every host handles the same way: tool switches, the colour
/// popup toggle, undo / redo / clear, and folding or dropping the text draft.
/// Returns false for ids the host must handle itself (完成, 取消, OCR, ...).
pub fn handle_common_toolbar(
    session: &mut Session,
    text: &mut Option<TextDraft>,
    color_open: &mut bool,
    id: &str,
) -> bool {
    // Undo/redo/cancel discard the draft instead, matching their keyboard paths.
    if matches!(id, toolbar::UNDO | toolbar::REDO | toolbar::CANCEL) {
        if text.take().is_some() {
            return true;
        }
    } else {
        commit_text_draft(session, text);
    }
    // Any button other than COLOR dismisses the popup; COLOR toggles it.
    *color_open = id == toolbar::COLOR && !*color_open;
    if let Some(tool) = Tool::from_id(id) {
        session.cancel_draft();
        session.tool = tool;
        return true;
    }
    match id {
        toolbar::COLOR => true,
        toolbar::UNDO => {
            session.undo();
            true
        }
        toolbar::REDO => {
            session.redo();
            true
        }
        toolbar::CLEAR => {
            session.clear();
            true
        }
        _ => false,
    }
}

/// Fold a pending text draft into the document. Any click that leaves the text
/// field -- canvas or toolbar -- has to run this or the typed text is lost.
pub fn commit_text_draft(session: &mut Session, text: &mut Option<TextDraft>) {
    if let Some(edit) = text.take() {
        session.add_text(edit.pt, &edit.buf);
    }
}

// ---- preview caches ---------------------------------------------------------

/// Identity of a mosaic readback: which elements, where, at what block size, and
/// how many path points -- extending a stroke inside its own bounds moves nothing else.
pub type MosaicKey = Vec<(u64, Rect, i32, usize)>;

#[derive(Default)]
pub struct MosaicTiles {
    pub tiles: Vec<(Rect, u32)>,
    pub covered: Vec<u64>,
}

/// Last mosaic readback, keyed by the elements that produced it. Hosts repaint
/// every frame; without this each mosaic costs a readback per frame. Shared via
/// `Rc` so a cache hit does not clone a tiles Vec that can hold thousands.
pub type MosaicCache = RefCell<Option<(MosaicKey, Rc<MosaicTiles>)>>;

/// Averaged mosaic cells for the visible elements. `readback(x, y, w, h)`
/// returns BGRA pixels of that edit-space rect (`pixels, w, h, stride`);
/// `extent` bounds the requests.
pub fn mosaic_tiles(
    cache: &MosaicCache,
    marks: &[Element],
    extent: (u32, u32),
    mut readback: impl FnMut(u32, u32, u32, u32) -> Option<(Vec<u8>, u32, u32, u32)>,
) -> Rc<MosaicTiles> {
    let mut out = MosaicTiles::default();
    let key: MosaicKey = marks
        .iter()
        .filter_map(|el| match &el.shape {
            Shape::Mosaic { points, block_size } => {
                Some((el.id, el.bounds, *block_size, points.len()))
            }
            _ => None,
        })
        .collect();
    if let Some((cached_key, tiles)) = cache.borrow().as_ref() {
        if *cached_key == key {
            return tiles.clone();
        }
    }
    for el in marks {
        let Shape::Mosaic { points, block_size } = &el.shape else {
            continue;
        };
        let block = (*block_size).max(2);
        let cells = crate::overlay::editor::mosaic_cells(points, block);
        if cells.is_empty() {
            continue;
        }
        // Read back one grid-aligned rect covering every cell, then average
        // inside it. Per-cell readbacks would be one round trip per tile.
        // ponytail: every new path point invalidates the cache and re-reads the
        // whole stroke bounds. Fine up to a few hundred cells; if smudging across
        // a full screen drops frames, cache per element id and only read back the
        // cells the newest segment added.
        let gx0 = cells.iter().map(|c| c.0).min().unwrap_or(0);
        let gy0 = cells.iter().map(|c| c.1).min().unwrap_or(0);
        let gx1 = cells.iter().map(|c| c.0).max().unwrap_or(0) + 1;
        let gy1 = cells.iter().map(|c| c.1).max().unwrap_or(0) + 1;
        let x = (gx0 * block).clamp(0, extent.0 as i32) as u32;
        let y = (gy0 * block).clamp(0, extent.1 as i32) as u32;
        let w = (gx1 * block).clamp(0, extent.0 as i32) as u32 - x;
        let h = (gy1 * block).clamp(0, extent.1 as i32) as u32 - y;
        if w < 1 || h < 1 {
            continue;
        }
        let Some((px, cw, ch, stride)) = readback(x, y, w, h) else {
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
    let out = Rc::new(out);
    *cache.borrow_mut() = Some((key, out.clone()));
    out
}

/// Cached coverage runs aligned 1:1 with the element list that produced them.
pub type MarkMasks = (Vec<Element>, Vec<Option<Rc<Vec<CoverageRun>>>>);
pub type MaskCache = RefCell<Option<MarkMasks>>;

/// Per-element coverage runs, cached against the exact element list. Preview
/// and export rasterize from the same masks (`composite::element_mask`), so
/// this cache is the only thing standing between a per-frame repaint and
/// re-rasterizing every stroke (and re-scanning it into runs) at 60 fps.
pub fn mark_masks(cache: &MaskCache, marks: &[Element]) -> Vec<Option<Rc<Vec<CoverageRun>>>> {
    if let Some((key, masks)) = cache.borrow().as_ref() {
        if key.as_slice() == marks {
            return masks.clone();
        }
    }
    let masks: Vec<Option<Rc<Vec<CoverageRun>>>> = marks
        .iter()
        .map(|el| element_mask(el).map(|m| Rc::new(m.runs())))
        .collect();
    *cache.borrow_mut() = Some((marks.to_vec(), masks.clone()));
    masks
}

// ---- painting ---------------------------------------------------------------

/// Paints the mosaic tiles and every non-text element. Text is a GPUI div in
/// the host's element tree (`text_layer`); the CPU composite handles export.
pub fn paint_marks(
    window: &mut Window,
    marks: &[Element],
    masks: &[Option<Rc<Vec<CoverageRun>>>],
    mosaic: &MosaicTiles,
    offset: Point,
    scale: f32,
) {
    for (r, color) in &mosaic.tiles {
        window.paint_quad(fill(rect_to_bounds_at(*r, offset, scale), rgb(*color)));
    }
    for (el, mask) in marks.iter().zip(masks.iter()) {
        match &el.shape {
            Shape::Mosaic { .. } => {
                if !mosaic.covered.contains(&el.id) {
                    window.paint_quad(fill(
                        rect_to_bounds_at(el.bounds, offset, scale),
                        gpui::hsla(0., 0., 0.5, 0.35),
                    ));
                }
            }
            Shape::Text { .. } => {}
            _ => {
                if let Some(runs) = mask {
                    paint_mask(window, runs, offset, scale, rgb(el.stroke & 0x00FF_FFFF));
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
fn paint_mask(
    window: &mut Window,
    runs: &[CoverageRun],
    offset: Point,
    scale: f32,
    color: gpui::Rgba,
) {
    for run in runs {
        let mut c = color;
        c.a *= run.alpha as f32 / 255.0;
        window.paint_quad(fill(
            Bounds {
                origin: point(
                    px((run.x0 as f64 - offset.x) as f32 / scale),
                    px((run.y as f64 - offset.y) as f32 / scale),
                ),
                size: size(px((run.x1 - run.x0) as f32 / scale), px(1.0 / scale)),
            },
            c,
        ));
    }
}

pub fn rect_to_bounds(r: Rect, scale: f32) -> Bounds<Pixels> {
    rect_to_bounds_at(r, Point { x: 0.0, y: 0.0 }, scale)
}

fn rect_to_bounds_at(r: Rect, offset: Point, scale: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            px((r.x - offset.x) as f32 / scale),
            px((r.y - offset.y) as f32 / scale),
        ),
        size: size(px(r.width as f32 / scale), px(r.height as f32 / scale)),
    }
}

// ---- text layer -------------------------------------------------------------

pub struct TextLayer<'a> {
    /// Logical, window-local box the text is clipped to.
    pub frame: Rect,
    /// Edit-space point at the frame's origin, physical px.
    pub offset: Point,
    pub scale: f32,
    pub elements: &'a [Element],
    pub draft: Option<&'a TextDraft>,
    /// Stroke colour the draft is drawn in.
    pub stroke: u32,
}

/// Committed text elements plus the draft, drawn exactly like the committed
/// element will be -- same origin, face, size and colour, no box -- so typing
/// is a live preview.
pub fn text_layer(layer: TextLayer<'_>) -> impl IntoElement {
    let TextLayer {
        frame,
        offset,
        scale,
        elements,
        draft,
        stroke,
    } = layer;
    let committed: Vec<_> = elements
        .iter()
        .filter_map(|el| {
            let Shape::Text { text, font_size } = &el.shape else {
                return None;
            };
            Some(
                div()
                    .absolute()
                    .left(px((el.bounds.x - offset.x) as f32 / scale))
                    .top(px((el.bounds.y - offset.y) as f32 / scale))
                    .font_family(text_raster::FONT_FAMILY)
                    .text_color(rgb(el.stroke & 0x00FF_FFFF))
                    .text_size(px(*font_size as f32 / scale))
                    .child(text.clone()),
            )
        })
        .collect();
    let draft = draft.map(|edit| {
        div()
            .absolute()
            .left(px((edit.pt.x - offset.x) as f32 / scale))
            .top(px((edit.pt.y - offset.y) as f32 / scale))
            .min_w(px(40.))
            .font_family(text_raster::FONT_FAMILY)
            .text_color(rgb(stroke & 0x00FF_FFFF))
            .text_size(px(TEXT_FONT_SIZE as f32 / scale))
            .occlude()
            .child(format!("{}|", edit.buf))
    });
    div()
        .absolute()
        .left(px(frame.x as f32))
        .top(px(frame.y as f32))
        .w(px(frame.width as f32))
        .h(px(frame.height as f32))
        .overflow_hidden()
        .children(committed)
        .children(draft)
}

// ---- toolbar ----------------------------------------------------------------

/// Callback for a toolbar button press, keyed by the button id.
pub type OnItem<V> = Rc<dyn Fn(&'static str, &mut V, &mut Window, &mut Context<V>)>;
/// Callback for a swatch pick: the ARGB stroke colour chosen.
pub type OnPick<V> = Rc<dyn Fn(u32, &mut V, &mut Window, &mut Context<V>)>;

pub struct ToolbarModel {
    /// Logical, window-local top-left of the bar.
    pub origin: (f32, f32),
    pub items: Vec<&'static str>,
    pub tool_id: &'static str,
    pub can_undo: bool,
    pub can_redo: bool,
    pub color_open: bool,
    pub stroke: u32,
}

fn chrome_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: theme::base().opacity(0.12),
        offset: point(px(0.), px(4.)),
        blur_radius: px(16.),
        spread_radius: px(0.),
    }]
}

pub fn toolbar_element<V: 'static>(
    model: ToolbarModel,
    on_item: OnItem<V>,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let ToolbarModel {
        origin,
        items,
        tool_id,
        can_undo,
        can_redo,
        color_open,
        stroke,
    } = model;
    div()
        .absolute()
        .left(px(origin.0))
        .top(px(origin.1))
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
        .shadow(chrome_shadow())
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .children(items.into_iter().map(|id| {
            let is_color = id == toolbar::COLOR;
            // The colour button tracks its popup, not the active tool.
            let active = if is_color { color_open } else { tool_id == id };
            let dim = (id == toolbar::UNDO && !can_undo) || (id == toolbar::REDO && !can_redo);
            toolbar_button(
                id,
                active,
                dim,
                is_color.then_some(stroke),
                on_item.clone(),
                cx,
            )
        }))
}

pub struct SwatchesModel {
    /// Logical, window-local top-left of the popup.
    pub origin: (f32, f32),
    pub current: u32,
}

/// The colour popup.
pub fn swatches_element<V: 'static>(
    model: SwatchesModel,
    on_pick: OnPick<V>,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let SwatchesModel { origin, current } = model;
    div()
        .absolute()
        .left(px(origin.0))
        .top(px(origin.1))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(toolbar::SWATCH_GAP as f32))
        .p(px(toolbar::PAD as f32))
        .rounded_xl()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .shadow(chrome_shadow())
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .children(theme::SWATCHES.map(|(hex, rgb24)| {
            let argb = 0xFF00_0000 | rgb24;
            let on_pick = on_pick.clone();
            div()
                .id(SharedString::from(hex))
                .w(px(toolbar::SWATCH as f32))
                .h(px(toolbar::SWATCH as f32))
                .rounded_full()
                .bg(rgb(rgb24))
                .border_2()
                .border_color(if current == argb {
                    theme::accent()
                } else {
                    theme::border()
                })
                .cursor(CursorStyle::PointingHand)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        on_pick(argb, this, window, cx);
                    }),
                )
        }))
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

/// `swatch` is `Some` only for COLOR, which shows the current stroke colour
/// instead of an icon.
fn toolbar_button<V: 'static>(
    id: &'static str,
    active: bool,
    dim: bool,
    swatch: Option<u32>,
    on_item: OnItem<V>,
    cx: &mut Context<V>,
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
        .child(match swatch {
            Some(argb) => div()
                .w(px(toolbar::SWATCH_ICON))
                .h(px(toolbar::SWATCH_ICON))
                .rounded_full()
                .bg(rgb(argb & 0xFF_FFFF))
                .border_1()
                .border_color(theme::border())
                .into_any_element(),
            None => svg()
                .path(toolbar::icon_path(id).unwrap_or(""))
                .w(px(toolbar::ICON))
                .h(px(toolbar::ICON))
                .text_color(fg)
                .into_any_element(),
        })
        .tooltip(move |_, cx| {
            let view: AnyView = cx.new(|_| ToolLabel(toolbar::label(id).into())).into();
            view
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, _, window, cx| {
                on_item(id, view, window, cx);
                cx.stop_propagation();
            }),
        )
}

/// Mirror the rendered layout -- `PAD`, then one `BTN` every `BTN + ITEM_GAP`.
/// Splitting `geom.width` into equal slots instead drifts by the padding and can
/// land a click on Cancel in Complete's slot.
pub fn toolbar_item_at(p: Point, geom: Rect, items: &[&'static str]) -> Option<&'static str> {
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

//! Pure geometry behind pin stitching. A stitched pin is a *group* of
//! rectangular pin windows that stay edge-to-edge and move together; nothing
//! is rasterized until the group is copied or saved. This module knows nothing
//! about windows: it snaps a dragged rectangle onto the exposed edges of other
//! tiles, scales a tile about an anchor so shared edges stay shared, and
//! composites a group's tiles into one BGRA image.

use crate::geom::Rect;

/// Axis-aligned rectangle in physical screen pixels, half-open on the right
/// and bottom like Win32 `RECT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl IRect {
    pub fn precise(self) -> Rect {
        Rect::new(
            self.left as f64,
            self.top as f64,
            self.width() as f64,
            self.height() as f64,
        )
    }
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn from_origin_size(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self::new(x, y, x + w, y + h)
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn area(&self) -> i64 {
        self.width().max(0) as i64 * self.height().max(0) as i64
    }

    pub fn overlap_area(&self, other: &IRect) -> i64 {
        let w = self.right.min(other.right) - self.left.max(other.left);
        let h = self.bottom.min(other.bottom) - self.top.max(other.top);
        if w > 0 && h > 0 {
            w as i64 * h as i64
        } else {
            0
        }
    }

    pub fn translated(&self, dx: i32, dy: i32) -> IRect {
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right + dx,
            self.bottom + dy,
        )
    }

    pub fn union(&self, other: &IRect) -> IRect {
        Self::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }
}

/// Which side of the target tile the moving rect attaches to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// One existing pin as seen by the snap search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub rect: IRect,
    pub group: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snap {
    /// Where the moving rect lands.
    pub rect: IRect,
    /// Index into the `tiles` slice passed to `find_snap`.
    pub target: usize,
    pub side: Side,
}

#[derive(Clone, Copy, Debug)]
pub struct SnapRules {
    /// Max edge gap (or penetration) that still snaps, physical px.
    pub tolerance: i32,
    /// Min length the two tiles must share along the snapped edge, so the
    /// group stays connected by edges, never by a single corner.
    pub min_shared: i32,
    /// Overlap with any tile above this fraction of the moving rect's area
    /// reads as "dropped on top of it": the user is stacking, not stitching.
    pub max_overlap: f32,
}

/// Snap target for `moving` among `tiles`. The best candidate is the one that
/// displaces the rect least; candidates that would overlap any tile of the
/// target's group are rejected, which is what makes only *exposed* edges
/// snappable.
pub fn find_snap(moving: IRect, tiles: &[Tile], rules: SnapRules) -> Option<Snap> {
    let area = moving.area();
    if area == 0 {
        return None;
    }
    let stacking = tiles
        .iter()
        .any(|t| moving.overlap_area(&t.rect) as f32 / area as f32 > rules.max_overlap);
    if stacking {
        return None;
    }

    let mut best: Option<(i32, Snap)> = None;
    for (i, t) in tiles.iter().enumerate() {
        let r = t.rect;
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            let (dx, dy) = match side {
                Side::Right => (r.right - moving.left, 0),
                Side::Left => (r.left - moving.right, 0),
                Side::Bottom => (0, r.bottom - moving.top),
                Side::Top => (0, r.top - moving.bottom),
            };
            let gap = dx.abs() + dy.abs();
            if gap > rules.tolerance {
                continue;
            }
            let mut cand = moving.translated(dx, dy);
            // Secondary alignment on the perpendicular axis: a near-aligned
            // top/left or bottom/right edge snaps too.
            let (ax, ay) = match side {
                Side::Left | Side::Right => {
                    if (cand.top - r.top).abs() <= rules.tolerance {
                        (0, r.top - cand.top)
                    } else if (cand.bottom - r.bottom).abs() <= rules.tolerance {
                        (0, r.bottom - cand.bottom)
                    } else {
                        (0, 0)
                    }
                }
                Side::Top | Side::Bottom => {
                    if (cand.left - r.left).abs() <= rules.tolerance {
                        (r.left - cand.left, 0)
                    } else if (cand.right - r.right).abs() <= rules.tolerance {
                        (r.right - cand.right, 0)
                    } else {
                        (0, 0)
                    }
                }
            };
            cand = cand.translated(ax, ay);

            let shared = match side {
                Side::Left | Side::Right => cand.bottom.min(r.bottom) - cand.top.max(r.top),
                Side::Top | Side::Bottom => cand.right.min(r.right) - cand.left.max(r.left),
            };
            if shared < rules.min_shared {
                continue;
            }
            let blocked = tiles
                .iter()
                .any(|o| o.group == t.group && cand.overlap_area(&o.rect) > 0);
            if blocked {
                continue;
            }
            let score = gap + ax.abs() + ay.abs();
            if best.is_none_or(|(s, _)| score < s) {
                best = Some((
                    score,
                    Snap {
                        rect: cand,
                        target: i,
                        side,
                    },
                ));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// True when `a` and `b` share an edge of at least `min_shared` px and do
/// not overlap: the geometric precondition for linking them.
pub fn are_adjacent(a: IRect, b: IRect, min_shared: i32) -> bool {
    if a.overlap_area(&b) > 0 {
        return false;
    }
    let shared_v = a.bottom.min(b.bottom) - a.top.max(b.top);
    let shared_h = a.right.min(b.right) - a.left.max(b.left);
    let touch_x = a.right == b.left || b.right == a.left;
    let touch_y = a.bottom == b.top || b.bottom == a.top;
    (touch_x && shared_v >= min_shared) || (touch_y && shared_h >= min_shared)
}

/// Scales each edge coordinate about `anchor` independently. Two tiles that
/// share an edge coordinate map it to the same rounded value, so a group
/// zoomed this way stays gap-free; scaling origin+size would not.
pub fn scale_edges(rect: IRect, anchor: (i32, i32), factor: f32) -> IRect {
    let s = |v: i32, a: i32| a + ((v - a) as f32 * factor).round() as i32;
    let left = s(rect.left, anchor.0);
    let top = s(rect.top, anchor.1);
    let right = s(rect.right, anchor.0).max(left + 1);
    let bottom = s(rect.bottom, anchor.1).max(top + 1);
    IRect::new(left, top, right, bottom)
}

/// Keep fractional edges between zoom steps; round only at the HWND boundary.
pub fn zoom_rect(rect: Rect, anchor: (i32, i32), factor: f64) -> Rect {
    Rect::new(
        anchor.0 as f64 + (rect.x - anchor.0 as f64) * factor,
        anchor.1 as f64 + (rect.y - anchor.1 as f64) * factor,
        rect.width * factor,
        rect.height * factor,
    )
}

pub fn rounded_rect(rect: Rect) -> IRect {
    let left = rect.x.round() as i32;
    let top = rect.y.round() as i32;
    IRect::new(
        left,
        top,
        (rect.right().round() as i32).max(left + 1),
        (rect.bottom().round() as i32).max(top + 1),
    )
}

/// One tile's pixels for compositing: tightly packed BGRA at the pin's
/// native size, and the screen rect it currently occupies.
pub struct TileImage<'a> {
    pub rect: Rect,
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub zoom: f32,
}

/// Largest canvas side `compose` will produce; matches the long-capture
/// budget and the D3D texture limit the result may be shown with.
pub const MAX_CANVAS_SIDE: i32 = 16384;

/// How a set of tiles maps onto one canvas: `k` scales screen pixels to
/// canvas pixels about `anchor` (the bounding box's top-left), and `canvas`
/// is the resulting rect in that scaled space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub k: f32,
    pub anchor: (f64, f64),
    pub canvas: IRect,
}

/// When every tile sits at the same zoom the canvas is laid out at native
/// resolution so nothing is resampled; mixed zooms stay in screen pixels.
pub fn layout(tiles: &[(Rect, f32)]) -> Option<Layout> {
    let (first, z0) = *tiles.first()?;
    let uniform = tiles.iter().all(|(_, z)| (z - z0).abs() < 1e-3 && *z > 0.0);
    let k = if uniform { 1.0 / z0 } else { 1.0 };
    let bbox = tiles.iter().skip(1).fold(first, |acc, (r, _)| {
        Rect::normalized(
            acc.x.min(r.x),
            acc.y.min(r.y),
            acc.right().max(r.right()),
            acc.bottom().max(r.bottom()),
        )
    });
    let anchor = (bbox.x, bbox.y);
    Some(Layout {
        k,
        anchor,
        canvas: IRect::from_origin_size(
            0,
            0,
            (bbox.width * k as f64).round().max(1.0) as i32,
            (bbox.height * k as f64).round().max(1.0) as i32,
        ),
    })
}

/// A tile's destination rect on the canvas, in canvas pixels.
pub fn tile_dest(rect: Rect, lay: &Layout) -> IRect {
    rounded_rect(Rect::new(
        (rect.x - lay.anchor.0) * lay.k as f64,
        (rect.y - lay.anchor.1) * lay.k as f64,
        rect.width * lay.k as f64,
        rect.height * lay.k as f64,
    ))
}

/// Composites the tiles at their screen layout into one tight BGRA image.
/// Gaps stay transparent; see `layout` for the resolution rule.
pub fn compose(tiles: &[TileImage]) -> Option<(Vec<u8>, u32, u32)> {
    let lay = layout(&tiles.iter().map(|t| (t.rect, t.zoom)).collect::<Vec<_>>())?;
    let canvas = lay.canvas;
    let (cw, ch) = (canvas.width(), canvas.height());
    if cw <= 0 || ch <= 0 || cw > MAX_CANVAS_SIDE || ch > MAX_CANVAS_SIDE {
        return None;
    }
    let (cw, ch) = (cw as u32, ch as u32);
    let row = cw as usize * 4;
    let mut out = vec![0u8; row * ch as usize];

    for t in tiles {
        if t.pixels.len() < (t.width * t.height * 4) as usize || t.width == 0 || t.height == 0 {
            continue;
        }
        let dest = tile_dest(t.rect, &lay);
        let (dw, dh) = (dest.width().max(1) as u32, dest.height().max(1) as u32);
        let resized;
        let src: &[u8] = if (dw, dh) == (t.width, t.height) {
            t.pixels
        } else {
            let img = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
                t.width,
                t.height,
                &t.pixels[..(t.width * t.height * 4) as usize],
            )?;
            resized = image::imageops::resize(&img, dw, dh, image::imageops::FilterType::Triangle);
            resized.as_raw()
        };
        blit(&mut out, (cw, ch), src, (dw, dh), dest.left, dest.top);
    }
    Some((out, cw, ch))
}

/// Copies `src` (tight BGRA, `src_size`) into `dst` (`dst_size`) at
/// (`x`,`y`), clipping to the canvas.
fn blit(dst: &mut [u8], dst_size: (u32, u32), src: &[u8], src_size: (u32, u32), x: i32, y: i32) {
    let (cw, ch) = dst_size;
    let (sw, sh) = src_size;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + sw as i32).min(cw as i32);
    let y1 = (y + sh as i32).min(ch as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let span = (x1 - x0) as usize * 4;
    for yy in y0..y1 {
        let sy = (yy - y) as usize;
        let sx = (x0 - x) as usize;
        let s = sy * sw as usize * 4 + sx * 4;
        let d = yy as usize * cw as usize * 4 + x0 as usize * 4;
        dst[d..d + span].copy_from_slice(&src[s..s + span]);
    }
}

/// Cuts `rect` out of a tight BGRA canvas. `None` when the rect leaves the
/// canvas or is empty.
pub fn crop(canvas: &[u8], cw: u32, ch: u32, rect: IRect) -> Option<Vec<u8>> {
    if rect.left < 0
        || rect.top < 0
        || rect.right > cw as i32
        || rect.bottom > ch as i32
        || rect.width() <= 0
        || rect.height() <= 0
    {
        return None;
    }
    let row = rect.width() as usize * 4;
    let mut out = Vec::with_capacity(row * rect.height() as usize);
    for y in rect.top..rect.bottom {
        let s = (y as usize * cw as usize + rect.left as usize) * 4;
        out.extend_from_slice(&canvas[s..s + row]);
    }
    Some(out)
}

/// Replaces fully transparent pixels with `fill`. The clipboard DIB has no
/// reliable alpha, so gaps go out white there while the PNG keeps them clear.
pub fn flatten_gaps(pixels: &mut [u8], fill: [u8; 4]) {
    // `pixels` is tightly packed BGRA, so the remainder is always empty.
    for px in pixels.as_chunks_mut::<4>().0 {
        if px[3] == 0 {
            px.copy_from_slice(&fill);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: SnapRules = SnapRules {
        tolerance: 12,
        min_shared: 16,
        max_overlap: 0.5,
    };

    fn tile(l: i32, t: i32, r: i32, b: i32, group: u64) -> Tile {
        Tile {
            rect: IRect::new(l, t, r, b),
            group,
        }
    }

    #[test]
    fn snaps_to_right_edge_within_tolerance() {
        let tiles = [tile(0, 0, 100, 100, 1)];
        // 5 px gap to the right, top nearly aligned -> both axes snap.
        let moving = IRect::from_origin_size(105, 3, 50, 50);
        let snap = find_snap(moving, &tiles, RULES).unwrap();
        assert_eq!(snap.side, Side::Right);
        assert_eq!(snap.rect, IRect::new(100, 0, 150, 50));
    }

    #[test]
    fn penetration_snaps_out_and_keeps_free_axis() {
        let tiles = [tile(0, 0, 100, 100, 1)];
        // Overlapping the left tile by 6 px, y well away from any edge.
        let moving = IRect::from_origin_size(94, 30, 50, 50);
        let snap = find_snap(moving, &tiles, RULES).unwrap();
        assert_eq!(snap.rect, IRect::new(100, 30, 150, 80));
    }

    #[test]
    fn far_away_does_not_snap() {
        let tiles = [tile(0, 0, 100, 100, 1)];
        assert!(find_snap(IRect::from_origin_size(130, 0, 50, 50), &tiles, RULES).is_none());
    }

    #[test]
    fn heavy_overlap_is_stacking() {
        let tiles = [tile(0, 0, 100, 100, 1)];
        // 40x50 of the 50x50 rect lies on the tile: the user dropped it on top.
        let moving = IRect::from_origin_size(60, 20, 50, 50);
        assert!(find_snap(moving, &tiles, RULES).is_none());
    }

    #[test]
    fn corner_touch_is_rejected() {
        let tiles = [tile(0, 0, 100, 100, 1)];
        // Right of the tile but only 8 px of shared edge.
        let moving = IRect::from_origin_size(104, 92, 50, 50);
        assert!(find_snap(moving, &tiles, RULES).is_none());
    }

    #[test]
    fn internal_edge_of_a_group_is_not_snappable() {
        // A and B side by side in one group; a rect dropped over their seam
        // from above must snap to the group's top, never to A's right edge.
        let tiles = [tile(0, 0, 100, 100, 1), tile(100, 0, 200, 100, 1)];
        let moving = IRect::from_origin_size(80, -55, 40, 50);
        let snap = find_snap(moving, &tiles, RULES).unwrap();
        assert_eq!(snap.side, Side::Top);
        assert_eq!(snap.rect, IRect::new(80, -50, 120, 0));
    }

    #[test]
    fn notch_of_a_staircase_is_snappable() {
        // A over B, B shifted right: the notch above B's right part is open.
        let tiles = [tile(0, 0, 100, 100, 1), tile(50, 100, 150, 200, 1)];
        // Drop into the notch, nearly aligned with B's right edge and A's
        // right edge; candidate lands on B's top.
        let moving = IRect::from_origin_size(103, 52, 50, 40);
        // A's right edge and B's top edge both resolve to the same corner.
        let snap = find_snap(moving, &tiles, RULES).unwrap();
        assert_eq!(snap.rect, IRect::new(100, 60, 150, 100));
    }

    #[test]
    fn prefers_smallest_displacement() {
        let tiles = [tile(0, 0, 100, 100, 1), tile(0, 110, 100, 210, 2)];
        // 2 px below A, 2 px above B before B's tolerance; A is nearer.
        let moving = IRect::from_origin_size(0, 102, 100, 6);
        let snap = find_snap(moving, &tiles, RULES).unwrap();
        assert_eq!(snap.target, 0);
        assert_eq!(snap.rect.top, 100);
    }

    #[test]
    fn adjacency_requires_shared_edge() {
        let a = IRect::new(0, 0, 100, 100);
        assert!(are_adjacent(a, IRect::new(100, 20, 150, 60), 16));
        assert!(!are_adjacent(a, IRect::new(101, 20, 150, 60), 16));
        assert!(!are_adjacent(a, IRect::new(100, 92, 150, 140), 16));
        assert!(!are_adjacent(a, IRect::new(90, 0, 150, 60), 16));
    }

    #[test]
    fn scale_edges_keeps_shared_edges_shared() {
        let a = IRect::new(0, 0, 33, 50);
        let b = IRect::new(33, 0, 70, 50);
        for f in [1.1f32, 1.0 / 1.1, 1.331, 0.75] {
            let sa = scale_edges(a, (17, 5), f);
            let sb = scale_edges(b, (17, 5), f);
            assert_eq!(sa.right, sb.left, "factor {f}");
            assert_eq!(sa.top, sb.top);
        }
    }

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect()
    }

    #[test]
    fn compose_native_when_zoom_uniform() {
        let red = solid(2, 2, [0, 0, 255, 255]);
        let blue = solid(2, 2, [255, 0, 0, 255]);
        // Both tiles zoomed 2x on screen (4x4 each), stacked vertically with a
        // 1-tile horizontal offset, so the canvas is 3x4 native with two
        // transparent corners.
        let tiles = [
            TileImage {
                rect: IRect::new(10, 10, 14, 14).precise(),
                pixels: &red,
                width: 2,
                height: 2,
                zoom: 2.0,
            },
            TileImage {
                rect: IRect::new(12, 14, 16, 18).precise(),
                pixels: &blue,
                width: 2,
                height: 2,
                zoom: 2.0,
            },
        ];
        let (px, w, h) = compose(&tiles).unwrap();
        assert_eq!((w, h), (3, 4));
        let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(at(0, 0), &[0, 0, 255, 255]);
        assert_eq!(at(2, 0), &[0, 0, 0, 0]);
        assert_eq!(at(2, 3), &[255, 0, 0, 255]);
        assert_eq!(at(0, 3), &[0, 0, 0, 0]);
    }

    #[test]
    fn repeated_zoom_preserves_native_pixels_and_shared_edges() {
        let pixels: Vec<u8> = (0..100 * 100)
            .flat_map(|i| [(i % 251) as u8, 7, 19, 255])
            .collect();
        let mut a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let mut b = Rect::new(100.0, 0.0, 100.0, 100.0);
        let mut zoom = 1.0f32;
        for _ in 0..14 {
            let next = zoom * (1.0f32 / 1.1);
            let factor = next as f64 / zoom as f64;
            a = zoom_rect(a, (0, 0), factor);
            b = zoom_rect(b, (0, 0), factor);
            zoom = next;
            assert_eq!(rounded_rect(a).right, rounded_rect(b).left);
        }
        let tiles = [a, b].map(|rect| TileImage {
            rect,
            pixels: &pixels,
            width: 100,
            height: 100,
            zoom,
        });
        let (out, w, h) = compose(&tiles).unwrap();
        assert_eq!((w, h), (200, 100));
        assert_eq!(
            crop(&out, w, h, IRect::new(0, 0, 100, 100)).unwrap(),
            pixels
        );
        assert_eq!(
            crop(&out, w, h, IRect::new(100, 0, 200, 100)).unwrap(),
            pixels
        );
        assert_eq!(
            rounded_rect(zoom_rect(a, (0, 0), 1.0 / zoom as f64)),
            IRect::new(0, 0, 100, 100)
        );
    }

    #[test]
    fn compose_resamples_mixed_zoom() {
        let red = solid(2, 2, [0, 0, 255, 255]);
        let blue = solid(2, 2, [255, 0, 0, 255]);
        let tiles = [
            TileImage {
                rect: IRect::new(0, 0, 2, 2).precise(),
                pixels: &red,
                width: 2,
                height: 2,
                zoom: 1.0,
            },
            TileImage {
                rect: IRect::new(2, 0, 6, 4).precise(),
                pixels: &blue,
                width: 2,
                height: 2,
                zoom: 2.0,
            },
        ];
        let (px, w, h) = compose(&tiles).unwrap();
        assert_eq!((w, h), (6, 4));
        assert_eq!(
            &px[(5 * 4) as usize..(5 * 4 + 4) as usize],
            &[255, 0, 0, 255]
        );
        assert_eq!(
            &px[((3 * 6) * 4) as usize..((3 * 6) * 4 + 4) as usize],
            &[0, 0, 0, 0]
        );
    }

    #[test]
    fn compose_rejects_oversized_canvas() {
        let red = solid(1, 1, [0, 0, 255, 255]);
        let tiles = [
            TileImage {
                rect: IRect::new(0, 0, 1, 1).precise(),
                pixels: &red,
                width: 1,
                height: 1,
                zoom: 1.0,
            },
            TileImage {
                rect: IRect::new(MAX_CANVAS_SIDE + 5, 0, MAX_CANVAS_SIDE + 6, 1).precise(),
                pixels: &red,
                width: 1,
                height: 1,
                zoom: 1.0,
            },
        ];
        assert!(compose(&tiles).is_none());
    }

    #[test]
    fn crop_cuts_the_requested_rect() {
        // 3x2 canvas, pixel value = index.
        let canvas: Vec<u8> = (0..6u8).flat_map(|i| [i, i, i, 255]).collect();
        let px = crop(&canvas, 3, 2, IRect::new(1, 0, 3, 2)).unwrap();
        assert_eq!(
            px,
            vec![1, 1, 1, 255, 2, 2, 2, 255, 4, 4, 4, 255, 5, 5, 5, 255]
        );
        assert!(crop(&canvas, 3, 2, IRect::new(2, 0, 4, 1)).is_none());
    }

    #[test]
    fn layout_native_for_uniform_zoom_and_screen_for_mixed() {
        let a = IRect::new(10, 10, 14, 14);
        let b = IRect::new(14, 10, 18, 14);
        let lay = layout(&[(a.precise(), 2.0), (b.precise(), 2.0)]).unwrap();
        assert_eq!(lay.canvas, IRect::new(0, 0, 4, 2));
        assert_eq!(tile_dest(b.precise(), &lay), IRect::new(2, 0, 4, 2));
        let lay = layout(&[(a.precise(), 2.0), (b.precise(), 1.0)]).unwrap();
        assert_eq!(lay.k, 1.0);
        assert_eq!(tile_dest(b.precise(), &lay), IRect::new(4, 0, 8, 4));
    }

    #[test]
    fn flatten_fills_only_transparent() {
        let mut px = vec![0, 0, 0, 0, 1, 2, 3, 255];
        flatten_gaps(&mut px, [255, 255, 255, 255]);
        assert_eq!(px, vec![255, 255, 255, 255, 1, 2, 3, 255]);
    }
}

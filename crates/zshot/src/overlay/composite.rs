//! CPU composite of annotations onto a BGRA8 crop. Export blends each element's
//! coverage mask into the crop; the overlay preview turns the SAME mask into
//! alpha-weighted quads, so live preview and saved image share one rasterization.

use crate::geom::{Point, Rect};
use crate::overlay::editor::{Element, Shape};
use crate::overlay::text_raster;

struct Bgra<'a> {
    pixels: &'a mut [u8],
    width: u32,
    height: u32,
    stride: u32,
}

pub fn apply_annotations(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    origin_x: f64,
    origin_y: f64,
    elements: &[Element],
) {
    let mut buf = Bgra {
        pixels,
        width,
        height,
        stride,
    };
    for el in elements {
        let mut local = el.clone();
        local.bounds.x -= origin_x;
        local.bounds.y -= origin_y;
        match &mut local.shape {
            Shape::Arrow { start, end, .. } => {
                start.x -= origin_x;
                start.y -= origin_y;
                end.x -= origin_x;
                end.y -= origin_y;
            }
            Shape::Pen { points } => {
                for p in points {
                    p.x -= origin_x;
                    p.y -= origin_y;
                }
            }
            _ => {}
        }
        draw_element(&mut buf, &local);
    }
}

fn draw_element(buf: &mut Bgra<'_>, el: &Element) {
    let color = argb_to_bgra(el.stroke);
    match &el.shape {
        Shape::Mosaic { points, block_size } => draw_mosaic(buf, points, *block_size),
        Shape::Text { text, font_size } => {
            draw_text(buf, el.bounds, text, *font_size, el.stroke);
        }
        // Rect, Arrow and Pen all rasterize through the shared coverage mask, so
        // what the overlay preview shows is what this export writes.
        _ => {
            if let Some(mask) = element_mask(el) {
                blend_mask(buf, &mask, color);
            }
        }
    }
}

fn argb_to_bgra(argb: u32) -> [u8; 4] {
    let a = ((argb >> 24) & 0xFF) as u8;
    let r = ((argb >> 16) & 0xFF) as u8;
    let g = ((argb >> 8) & 0xFF) as u8;
    let b = (argb & 0xFF) as u8;
    [b, g, r, a]
}

impl Bgra<'_> {
    fn put(&mut self, x: i32, y: i32, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = y as usize * self.stride as usize + x as usize * 4;
        if i + 3 >= self.pixels.len() {
            return;
        }
        self.pixels[i] = c[0];
        self.pixels[i + 1] = c[1];
        self.pixels[i + 2] = c[2];
        self.pixels[i + 3] = c[3];
    }

    /// Source-over `c` at `coverage` (0..=1) opacity.
    fn blend(&mut self, x: i32, y: i32, c: [u8; 4], coverage: f64) {
        let a = coverage * c[3] as f64 / 255.0;
        if a >= 1.0 {
            self.put(x, y, c);
            return;
        }
        let dst = self.get(x, y);
        let mix = |s: u8, d: u8| (s as f64 * a + d as f64 * (1.0 - a)).round() as u8;
        let out = [
            mix(c[0], dst[0]),
            mix(c[1], dst[1]),
            mix(c[2], dst[2]),
            mix(c[3], dst[3]),
        ];
        self.put(x, y, out);
    }

    fn get(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0, 0, 0, 0];
        }
        let i = y as usize * self.stride as usize + x as usize * 4;
        if i + 3 >= self.pixels.len() {
            return [0, 0, 0, 0];
        }
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// Per-pixel coverage of one element, in the physical-pixel space the export
/// composite samples. The overlay preview converts this same mask into
/// alpha-weighted quads, one per constant-coverage run, so the live preview and
/// the exported crop are the same rasterization on different output targets.
pub struct CoverageMask {
    pub x0: i32,
    pub y0: i32,
    pub width: usize,
    pub height: usize,
    /// Row-major coverage: 0 = untouched, 255 = fully covered.
    pub data: Vec<u8>,
}

/// One horizontal span of equal coverage on a physical pixel row. Merging
/// equal-coverage neighbours keeps the preview's quad count proportional to the
/// edge length, not the filled area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoverageRun {
    pub y: i32,
    pub x0: i32,
    pub x1: i32,
    pub alpha: u8,
}

impl CoverageMask {
    /// Runs of equal coverage, row by row. Runs never overlap and every pixel
    /// they cover lies on the mask's integer pixel grid.
    pub fn runs(&self) -> Vec<CoverageRun> {
        let mut out = Vec::new();
        for row in 0..self.height {
            let base = row * self.width;
            let y = self.y0 + row as i32;
            let mut x = 0;
            while x < self.width {
                let a = self.data[base + x];
                if a == 0 {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < self.width && self.data[base + x] == a {
                    x += 1;
                }
                out.push(CoverageRun {
                    y,
                    x0: self.x0 + start as i32,
                    x1: self.x0 + x as i32,
                    alpha: a,
                });
            }
        }
        out
    }
}

/// Coverage of a single element, or None for shapes with no vector coverage
/// (mosaic smudges pixels, text is a GPU-side div in the overlay).
pub fn element_mask(el: &Element) -> Option<CoverageMask> {
    match &el.shape {
        Shape::Rect => stroke_mask(&rect_outline_points(el.bounds), el.stroke_width),
        Shape::Arrow { start, end, .. } => {
            let g = crate::overlay::editor::filled_arrow(*start, *end, el.stroke_width)?;
            triangles_mask(&g.triangles())
        }
        Shape::Pen { points } => stroke_mask(points, el.stroke_width),
        Shape::Mosaic { .. } | Shape::Text { .. } => None,
    }
}

fn rect_outline_points(r: Rect) -> [Point; 5] {
    let p0 = Point { x: r.x, y: r.y };
    let p1 = Point {
        x: r.x + r.width,
        y: r.y,
    };
    let p2 = Point {
        x: r.x + r.width,
        y: r.y + r.height,
    };
    let p3 = Point {
        x: r.x,
        y: r.y + r.height,
    };
    [p0, p1, p2, p3, p0]
}

/// Antialiased stroke coverage: round-capped capsules, coverage from the distance
/// to each segment, so caps and joints come out round. Coverage is accumulated
/// into the mask with max, so overlapping segments (a pen stroke samples far
/// denser than its own width) do not darken each other's soft edges.
///
/// A lone point is a segment of zero length -- `dist_to_segment` handles that,
/// so a single dot needs no separate case.
pub fn stroke_mask(pts: &[Point], thick: f64) -> Option<CoverageMask> {
    let first = pts.first()?;
    let r = (thick.max(1.0) / 2.0).max(0.5);
    let pad = r + 1.0;
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for p in pts {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let x0 = (min_x - pad).floor() as i32;
    let y0 = (min_y - pad).floor() as i32;
    let x1 = (max_x + pad).ceil() as i32;
    let y1 = (max_y + pad).ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let width = (x1 - x0) as usize;
    let height = (y1 - y0) as usize;
    let mut data = vec![0u8; width * height];
    let segments: Vec<(Point, Point)> = if pts.len() == 1 {
        vec![(*first, *first)]
    } else {
        pts.windows(2).map(|s| (s[0], s[1])).collect()
    };
    for (a, b) in segments {
        let sx0 = ((a.x.min(b.x) - pad).floor() as i32).clamp(x0, x1);
        let sy0 = ((a.y.min(b.y) - pad).floor() as i32).clamp(y0, y1);
        let sx1 = ((a.x.max(b.x) + pad).ceil() as i32).clamp(x0, x1);
        let sy1 = ((a.y.max(b.y) + pad).ceil() as i32).clamp(y0, y1);
        for y in sy0..sy1 {
            for x in sx0..sx1 {
                let p = Point {
                    x: x as f64 + 0.5,
                    y: y as f64 + 0.5,
                };
                let d = crate::overlay::editor::dist_to_segment(p, a, b);
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = (y - y0) as usize * width + (x - x0) as usize;
                data[i] = data[i].max((cov * 255.0).round() as u8);
            }
        }
    }
    Some(CoverageMask {
        x0,
        y0,
        width,
        height,
        data,
    })
}

/// Coverage of the union of `tris` from a 4x4 box supersample. Enough for
/// straight edges; go analytic only if 16 samples ever show banding on a
/// near-horizontal edge. Sampling the union in one pass keeps the arrow's
/// shared neck edge from blending twice.
pub fn triangles_mask(tris: &[(Point, Point, Point)]) -> Option<CoverageMask> {
    const S: i32 = 4;
    if tris.is_empty() {
        return None;
    }
    let mut min = Point {
        x: f64::MAX,
        y: f64::MAX,
    };
    let mut max = Point {
        x: f64::MIN,
        y: f64::MIN,
    };
    for (a, b, c) in tris {
        for p in [a, b, c] {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
    }
    if min.x > max.x {
        return None;
    }
    let x0 = min.x.floor() as i32;
    let y0 = min.y.floor() as i32;
    let x1 = max.x.ceil() as i32;
    let y1 = max.y.ceil() as i32;
    let width = (x1 - x0 + 1) as usize;
    let height = (y1 - y0 + 1) as usize;
    let mut data = vec![0u8; width * height];
    for y in y0..=y1 {
        for x in x0..=x1 {
            let mut hits = 0;
            for sy in 0..S {
                for sx in 0..S {
                    let px = x as f64 + (sx as f64 + 0.5) / S as f64;
                    let py = y as f64 + (sy as f64 + 0.5) / S as f64;
                    if tris
                        .iter()
                        .any(|(a, b, c)| point_in_triangle(px, py, *a, *b, *c))
                    {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let cov = hits as f64 / (S * S) as f64;
                data[(y - y0) as usize * width + (x - x0) as usize] = (cov * 255.0).round() as u8;
            }
        }
    }
    Some(CoverageMask {
        x0,
        y0,
        width,
        height,
        data,
    })
}

/// Source-over blend of a coverage mask into the crop. One blend per pixel,
/// matching the max-accumulated mask the preview draws as disjoint quads.
fn blend_mask(buf: &mut Bgra<'_>, mask: &CoverageMask, c: [u8; 4]) {
    for row in 0..mask.height {
        let y = mask.y0 + row as i32;
        if y < 0 || y >= buf.height as i32 {
            continue;
        }
        for col in 0..mask.width {
            let m = mask.data[row * mask.width + col];
            if m > 0 {
                buf.blend(mask.x0 + col as i32, y, c, m as f64 / 255.0);
            }
        }
    }
}

fn point_in_triangle(px: f64, py: f64, a: Point, b: Point, c: Point) -> bool {
    let d1 = (px - b.x) * (a.y - b.y) - (a.x - b.x) * (py - b.y);
    let d2 = (px - c.x) * (b.y - c.y) - (b.x - c.x) * (py - c.y);
    let d3 = (px - a.x) * (c.y - a.y) - (c.x - a.x) * (py - a.y);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// Flatten each grid cell the smudge path covers to that cell's mean colour.
/// Cells are image-aligned and never overlap, so writing them back in place cannot
/// contaminate a later cell's average.
fn draw_mosaic(buf: &mut Bgra<'_>, points: &[Point], block_size: i32) {
    let block = block_size.max(2);
    for (gx, gy) in crate::overlay::editor::mosaic_cells(points, block) {
        let x0 = (gx * block).clamp(0, buf.width as i32);
        let y0 = (gy * block).clamp(0, buf.height as i32);
        let x1 = (gx * block + block).clamp(0, buf.width as i32);
        let y1 = (gy * block + block).clamp(0, buf.height as i32);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let mut sum = [0u32; 4];
        let mut n = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let p = buf.get(x, y);
                for i in 0..4 {
                    sum[i] += p[i] as u32;
                }
                n += 1;
            }
        }
        if n == 0 {
            continue;
        }
        let avg = [
            (sum[0] / n) as u8,
            (sum[1] / n) as u8,
            (sum[2] / n) as u8,
            (sum[3] / n) as u8,
        ];
        for y in y0..y1 {
            for x in x0..x1 {
                buf.put(x, y, avg);
            }
        }
    }
}

/// Rasterizes `text` through DirectWrite/Direct2D (the same face as the overlay
/// preview) and composites the premultiplied result at `bounds`'s origin. The
/// raster's own size defines the extent -- unlike the old placeholder there is
/// no `bounds.width` clamp, matching the preview, which does not clip either.
fn draw_text(buf: &mut Bgra<'_>, bounds: Rect, text: &str, font_size: f64, argb: u32) {
    let (px_data, w, h) = match text_raster::rasterize(text, font_size, argb) {
        Ok(v) => v,
        Err(e) => {
            crate::perf::log_app(&format!("text raster failed: {e}"));
            return;
        }
    };
    let x0 = bounds.x.floor() as i32;
    let y0 = bounds.y.floor() as i32;
    let stride = (w * 4) as usize;
    for row in 0..h as usize {
        for col in 0..w as usize {
            let i = row * stride + col * 4;
            let src = [
                px_data[i],
                px_data[i + 1],
                px_data[i + 2],
                px_data[i + 3],
            ];
            if src[3] == 0 {
                continue;
            }
            let x = x0 + col as i32;
            let y = y0 + row as i32;
            let dst = buf.get(x, y);
            let inv = 1.0 - src[3] as f64 / 255.0;
            let mix = |s: u8, d: u8| (s as f64 + d as f64 * inv).round() as u8;
            buf.put(
                x,
                y,
                [
                    mix(src[0], dst[0]),
                    mix(src[1], dst[1]),
                    mix(src[2], dst[2]),
                    mix(src[3], dst[3]),
                ],
            );
        }
    }
}

/// Same helper Complete/Save uses after GPU crop: annotate then PNG-encode.
#[cfg(test)]
pub fn export_annotated_png(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    origin_x: f64,
    origin_y: f64,
    elements: &[Element],
) -> crate::error::Result<Vec<u8>> {
    let mut buf = pixels.to_vec();
    apply_annotations(
        &mut buf, width, height, stride, origin_x, origin_y, elements,
    );
    crate::codec::encode_png_bgra(&buf, width, height, stride)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::editor::{Element, Shape, DEFAULT_MOSAIC_BLOCK};
    use crate::overlay::theme::STROKE_ARGB;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    fn solid(w: u32, h: u32, c: [u8; 4]) -> (Vec<u8>, u32) {
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * stride + x * 4) as usize;
                px[i..i + 4].copy_from_slice(&c);
            }
        }
        (px, stride)
    }

    fn unique(w: u32, h: u32) -> (Vec<u8>, u32) {
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * stride + x * 4) as usize;
                px[i] = x as u8;
                px[i + 1] = y as u8;
                px[i + 2] = (x + y) as u8;
                px[i + 3] = 255;
            }
        }
        (px, stride)
    }

    fn rect_el(bounds: Rect) -> Element {
        Element {
            id: 1,
            bounds,
            stroke: STROKE_ARGB,
            stroke_width: 3.0,
            shape: Shape::Rect,
        }
    }

    /// A smudge stroke sweeping left-to-right across the middle of `bounds`.
    fn mosaic_el(bounds: Rect) -> Element {
        let y = bounds.y + bounds.height / 2.0;
        Element {
            id: 2,
            bounds,
            stroke: STROKE_ARGB,
            stroke_width: 1.0,
            shape: Shape::Mosaic {
                points: vec![
                    crate::geom::Point { x: bounds.x, y },
                    crate::geom::Point {
                        x: bounds.x + bounds.width,
                        y,
                    },
                ],
                block_size: DEFAULT_MOSAIC_BLOCK,
            },
        }
    }

    #[test]
    fn mosaic_flattens_cells_under_the_stroke_and_leaves_the_rest() {
        let (mut px, stride) = unique(64, 64);
        let original = px.clone();
        // One dab at (32,32). With block 16 the brush radius is 24, so the only
        // cell centres within reach are (24,24) (40,24) (24,40) (40,40) -- the four
        // cells spanning x 16..48, y 16..48.
        let el = Element {
            id: 2,
            bounds: Rect::new(8.0, 8.0, 48.0, 48.0),
            stroke: STROKE_ARGB,
            stroke_width: 1.0,
            shape: Shape::Mosaic {
                points: vec![crate::geom::Point { x: 32.0, y: 32.0 }],
                block_size: 16,
            },
        };
        apply_annotations(&mut px, 64, 64, stride, 0.0, 0.0, &[el]);
        let at = |buf: &[u8], x: u32, y: u32| {
            let i = (y * stride + x * 4) as usize;
            buf[i..i + 4].to_vec()
        };
        for y in 0..64u32 {
            for x in 0..64u32 {
                let touched = (16..48).contains(&x) && (16..48).contains(&y);
                if !touched {
                    assert_eq!(at(&px, x, y), at(&original, x, y), "untouched {x},{y}");
                }
            }
        }
        for (cx, cy) in [(16u32, 16u32), (32, 16), (16, 32), (32, 32)] {
            let sample = at(&px, cx, cy);
            for y in cy..cy + 16 {
                for x in cx..cx + 16 {
                    assert_eq!(at(&px, x, y), sample, "cell at {cx},{cy}: {x},{y}");
                }
            }
            assert_ne!(sample, at(&original, cx, cy), "cell {cx},{cy} unchanged");
        }
    }

    /// The point of image-aligned cells: whatever offset the stroke starts at, every
    /// 16x16 cell of the image is either untouched or flattened whole. A grid that
    /// followed the stroke would leave cells half-changed or holding two colours.
    #[test]
    fn mosaic_cells_stay_on_the_image_grid_whatever_the_stroke_offset() {
        for start_x in [16.0, 21.0, 28.0, 33.5] {
            let (mut px, stride) = unique(96, 64);
            let original = px.clone();
            let el = Element {
                id: 2,
                bounds: Rect::new(0.0, 0.0, 96.0, 64.0),
                stroke: STROKE_ARGB,
                stroke_width: 1.0,
                shape: Shape::Mosaic {
                    points: vec![
                        crate::geom::Point {
                            x: start_x,
                            y: 30.0,
                        },
                        crate::geom::Point {
                            x: start_x + 30.0,
                            y: 34.0,
                        },
                    ],
                    block_size: 16,
                },
            };
            apply_annotations(&mut px, 96, 64, stride, 0.0, 0.0, &[el]);
            let at = |buf: &[u8], x: u32, y: u32| {
                let i = (y * stride + x * 4) as usize;
                buf[i..i + 4].to_vec()
            };
            for gy in 0..4u32 {
                for gx in 0..6u32 {
                    let (x0, y0) = (gx * 16, gy * 16);
                    let cells: Vec<Vec<u8>> = (y0..y0 + 16)
                        .flat_map(|y| (x0..x0 + 16).map(move |x| (x, y)))
                        .map(|(x, y)| at(&px, x, y))
                        .collect();
                    let untouched = (y0..y0 + 16)
                        .flat_map(|y| (x0..x0 + 16).map(move |x| (x, y)))
                        .all(|(x, y)| at(&px, x, y) == at(&original, x, y));
                    if untouched {
                        continue;
                    }
                    assert!(
                        cells.windows(2).all(|w| w[0] == w[1]),
                        "start_x {start_x}: cell {gx},{gy} is not uniform"
                    );
                }
            }
        }
    }

    #[test]
    fn annotated_png_differs_from_unmarked_crop() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (plain, stride) = solid(32, 32, [10, 20, 30, 255]);
        let unmarked = crate::codec::encode_png_bgra(&plain, 32, 32, stride).expect("png");
        let marked = export_annotated_png(
            &plain,
            32,
            32,
            stride,
            0.0,
            0.0,
            &[rect_el(Rect::new(4.0, 4.0, 20.0, 16.0))],
        )
        .expect("annotated png");
        assert_ne!(marked, unmarked);
        assert_eq!(&marked[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    }

    #[test]
    fn mosaic_export_png_differs_and_cells_are_constant() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (src, stride) = unique(32, 32);
        let unmarked = crate::codec::encode_png_bgra(&src, 32, 32, stride).expect("png");
        let mut marked_px = src.clone();
        apply_annotations(
            &mut marked_px,
            32,
            32,
            stride,
            0.0,
            0.0,
            &[mosaic_el(Rect::new(0.0, 0.0, 32.0, 32.0))],
        );
        let marked = crate::codec::encode_png_bgra(&marked_px, 32, 32, stride).expect("ann");
        assert_ne!(marked, unmarked);
        for by in [0i32, 16] {
            for bx in [0i32, 16] {
                let i0 = (by as u32 * stride + bx as u32 * 4) as usize;
                let sample = marked_px[i0..i0 + 4].to_vec();
                for y in by..by + 16 {
                    for x in bx..bx + 16 {
                        let i = (y as u32 * stride + x as u32 * 4) as usize;
                        assert_eq!(&marked_px[i..i + 4], sample.as_slice());
                    }
                }
            }
        }
    }

    #[test]
    fn mosaic_reaching_past_the_crop_leaves_pixels_opaque() {
        let (mut px, stride) = solid(32, 32, [10, 20, 30, 255]);
        let el = Element {
            id: 9,
            bounds: Rect::new(-8.0, 8.0, 16.0, 16.0),
            stroke: STROKE_ARGB,
            stroke_width: 1.0,
            shape: Shape::Mosaic {
                points: vec![crate::geom::Point { x: -8.0, y: 16.0 }],
                block_size: 16,
            },
        };
        apply_annotations(&mut px, 32, 32, stride, 0.0, 0.0, &[el]);
        for y in 0..32u32 {
            for x in 0..32u32 {
                let i = (y * stride + x * 4) as usize;
                assert_eq!(px[i + 3], 255, "pixel {x},{y} went transparent");
            }
        }
    }

    #[test]
    fn mask_runs_merge_equal_coverage_and_skip_gaps() {
        let mask = CoverageMask {
            x0: 10,
            y0: 20,
            width: 4,
            height: 2,
            data: vec![0, 7, 7, 0, 3, 0, 9, 9],
        };
        assert_eq!(
            mask.runs(),
            vec![
                CoverageRun {
                    y: 20,
                    x0: 11,
                    x1: 13,
                    alpha: 7,
                },
                CoverageRun {
                    y: 21,
                    x0: 10,
                    x1: 11,
                    alpha: 3,
                },
                CoverageRun {
                    y: 21,
                    x0: 12,
                    x1: 14,
                    alpha: 9,
                },
            ]
        );
    }

    /// The preview draws `element_mask` as quads; the export blends the same mask.
    /// A slanted stroke must therefore have partial coverage at its edges -- the
    /// exact pixels that used to stair-step when the preview scanlined instead.
    #[test]
    fn slanted_arrow_mask_has_soft_edges_in_both_axes() {
        for angle_deg in [0.0_f64, 15.0, 30.0, 45.0, 60.0, 75.0] {
            let rad = angle_deg.to_radians();
            let el = Element {
                id: 5,
                bounds: Rect::new(0.0, 0.0, 64.0, 64.0),
                stroke: 0xFFFF_FFFF,
                stroke_width: 3.0,
                shape: Shape::Arrow {
                    start: crate::geom::Point { x: 8.0, y: 32.0 },
                    end: crate::geom::Point {
                        x: 8.0 + 48.0 * rad.cos(),
                        y: 32.0 + 48.0 * rad.sin(),
                    },
                    head: 12.0,
                },
            };
            let mask = element_mask(&el).expect("arrow mask");
            let partial = mask.data.iter().filter(|&&m| m > 0 && m < 255).count();
            let solid = mask.data.iter().filter(|&&m| m == 255).count();
            assert!(solid > 10, "{angle_deg}°: no solid interior");
            assert!(
                partial > 20,
                "{angle_deg}°: expected soft edge pixels, got {partial}"
            );
            let runs = mask.runs();
            for w in runs.windows(2) {
                if w[0].y == w[1].y {
                    assert!(w[0].x1 <= w[1].x0, "runs overlap: {:?} {:?}", w[0], w[1]);
                }
            }
        }
    }

    /// Preview and export rasterize from one mask: white-on-black export pixels
    /// must equal the mask value the preview turns into quad alpha, per pixel.
    #[test]
    fn preview_mask_matches_export_pixels() {
        let el = Element {
            id: 6,
            bounds: Rect::new(0.0, 0.0, 64.0, 64.0),
            stroke: 0xFFFF_FFFF,
            stroke_width: 4.0,
            shape: Shape::Pen {
                points: vec![
                    crate::geom::Point { x: 6.0, y: 6.0 },
                    crate::geom::Point { x: 40.0, y: 30.0 },
                    crate::geom::Point { x: 20.0, y: 56.0 },
                ],
            },
        };
        let (mut px, stride) = solid(64, 64, [0, 0, 0, 255]);
        apply_annotations(&mut px, 64, 64, stride, 0.0, 0.0, std::slice::from_ref(&el));
        let mask = element_mask(&el).expect("pen mask");
        for row in 0..mask.height {
            for col in 0..mask.width {
                let m = mask.data[row * mask.width + col];
                let x = (mask.x0 + col as i32) as u32;
                let y = (mask.y0 + row as i32) as u32;
                let i = (y * stride + x * 4) as usize;
                assert_eq!(px[i], m, "pixel {x},{y}: export vs preview mask");
            }
        }
    }

    #[test]
    fn pen_stroke_edges_are_antialiased() {
        let (mut px, stride) = solid(64, 64, [0, 0, 0, 255]);
        let el = Element {
            id: 7,
            bounds: Rect::new(0.0, 0.0, 64.0, 64.0),
            stroke: 0xFFFF_FFFF,
            stroke_width: 4.0,
            shape: Shape::Pen {
                points: vec![
                    crate::geom::Point { x: 6.0, y: 6.0 },
                    crate::geom::Point { x: 40.0, y: 30.0 },
                    crate::geom::Point { x: 20.0, y: 56.0 },
                ],
            },
        };
        apply_annotations(&mut px, 64, 64, stride, 0.0, 0.0, &[el]);
        let partial = px
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 0 && p[0] < 255)
            .count();
        let solid_px = px.as_chunks::<4>().0.iter().filter(|p| p[0] == 255).count();
        assert!(solid_px > 50, "stroke barely drawn: {solid_px}");
        assert!(partial > 40, "expected soft edge pixels, got {partial}");
    }

    #[test]
    fn filled_arrow_edges_are_antialiased() {
        let (mut px, stride) = solid(64, 32, [0, 0, 0, 255]);
        let el = Element {
            id: 4,
            bounds: Rect::new(2.0, 2.0, 58.0, 28.0),
            stroke: 0xFFFF_FFFF,
            stroke_width: 3.0,
            shape: Shape::Arrow {
                // Slanted, so every edge crosses pixels partially.
                start: crate::geom::Point { x: 4.0, y: 4.0 },
                end: crate::geom::Point { x: 60.0, y: 28.0 },
                head: 12.0,
            },
        };
        apply_annotations(&mut px, 64, 32, stride, 0.0, 0.0, &[el]);
        let partial = px
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 0 && p[0] < 255)
            .count();
        assert!(partial > 20, "expected soft edge pixels, got {partial}");
    }

    #[test]
    fn filled_arrow_head_is_wider_than_shaft() {
        let (mut px, stride) = solid(64, 32, [0, 0, 0, 255]);
        let el = Element {
            id: 3,
            bounds: Rect::new(2.0, 2.0, 58.0, 28.0),
            stroke: STROKE_ARGB,
            stroke_width: 3.0,
            shape: Shape::Arrow {
                start: crate::geom::Point { x: 4.0, y: 16.0 },
                end: crate::geom::Point { x: 60.0, y: 16.0 },
                head: 12.0,
            },
        };
        apply_annotations(&mut px, 64, 32, stride, 0.0, 0.0, &[el]);
        let col = |x: u32| {
            (0..32u32)
                .filter(|&y| {
                    let i = (y * stride + x * 4) as usize;
                    px[i] != 0 || px[i + 1] != 0 || px[i + 2] != 0
                })
                .count()
        };
        let tail = col(8);
        let neck = col(40);
        let head = col(46);
        assert!(tail >= 1, "tail too thin: {tail}");
        assert!(
            neck > tail + 2,
            "body should taper: tail {tail} neck {neck}"
        );
        assert!(head > neck, "head {head} should be wider than neck {neck}");
    }

    /// Real glyphs, not the old placeholder block: antialiased edges and a mix
    /// of opaque/transparent pixels, not a solid rectangle.
    #[test]
    fn text_export_draws_real_glyphs() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (buf, w, h) = text_raster::rasterize("Hi 你好 123", 24.0, 0xFFFF_FFFF).expect("raster");
        assert!(w >= 40, "width too small: {w}");
        assert!(h >= 20, "height too small: {h}");
        let total = (w * h) as usize;
        let mut opaque = 0;
        let mut partial = 0;
        let mut transparent = 0;
        for px in buf.chunks_exact(4) {
            match px[3] {
                255 => opaque += 1,
                0 => transparent += 1,
                _ => partial += 1,
            }
        }
        assert!(opaque > 0, "no opaque pixels");
        assert!(partial > 0, "no antialiased edge pixels");
        assert!(opaque < total / 2, "looks like a solid block: {opaque}/{total}");
        assert!(
            transparent as f64 > total as f64 * 0.3,
            "too little background: {transparent}/{total}"
        );
    }

    /// The raster lands at the element's bounds and nowhere else in the crop.
    #[test]
    fn text_annotation_lands_in_the_crop() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (mut px, stride) = solid(200, 64, [0, 0, 0, 255]);
        let original = px.clone();
        let el = Element {
            id: 10,
            bounds: Rect::new(10.0, 10.0, 140.0, 30.0),
            stroke: 0xFFFF_FFFF,
            stroke_width: 1.0,
            shape: Shape::Text {
                text: "Zshot".to_string(),
                font_size: 20.0,
            },
        };
        apply_annotations(&mut px, 200, 64, stride, 0.0, 0.0, &[el]);
        let at = |buf: &[u8], x: u32, y: u32| {
            let i = (y * stride + x * 4) as usize;
            buf[i..i + 4].to_vec()
        };
        let mut changed_in_box = false;
        for y in 0..64u32 {
            for x in 0..200u32 {
                let inside = (10..150).contains(&x) && (10..40).contains(&y);
                let differs = at(&px, x, y) != at(&original, x, y);
                if inside {
                    changed_in_box |= differs;
                } else if !(0..60).contains(&y) {
                    assert!(!differs, "pixel {x},{y} changed far from the text");
                }
            }
        }
        assert!(changed_in_box, "no pixels changed inside the text bounds");
    }
}

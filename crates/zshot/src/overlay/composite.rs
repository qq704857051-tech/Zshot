//! CPU composite of annotations onto a BGRA8 crop. Overlay paint and export share this.

use crate::geom::{Point, Rect};
use crate::overlay::editor::{Element, Shape};

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
    let thick = el.stroke_width.max(1.0);
    match &el.shape {
        Shape::Rect => draw_rect_outline(buf, el.bounds, color, thick),
        Shape::Arrow { start, end, .. } => {
            if let Some(g) = crate::overlay::editor::filled_arrow(*start, *end, thick) {
                fill_triangles(buf, &g.triangles(), color);
            }
        }
        Shape::Pen { points } => {
            if points.len() == 1 {
                fill_disk(buf, points[0].x, points[0].y, thick / 2.0, color);
            } else {
                for w in points.windows(2) {
                    draw_line(buf, w[0], w[1], color, thick);
                }
            }
        }
        Shape::Mosaic { block_size } => {
            pixelate_buf(buf, el.bounds, (*block_size).max(2));
        }
        Shape::Text { text, font_size } => {
            draw_text(buf, el.bounds, text, *font_size, color);
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

fn fill_disk(buf: &mut Bgra<'_>, cx: f64, cy: f64, radius: f64, c: [u8; 4]) {
    let r = radius.max(0.5);
    let min_x = (cx - r).floor() as i32;
    let max_x = (cx + r).ceil() as i32;
    let min_y = (cy - r).floor() as i32;
    let max_y = (cy + r).ceil() as i32;
    let r2 = r * r;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f64 + 0.5 - cx;
            let dy = y as f64 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                buf.put(x, y, c);
            }
        }
    }
}

fn draw_line(buf: &mut Bgra<'_>, a: Point, b: Point, c: [u8; 4], thick: f64) {
    let mut x0 = a.x.round() as i32;
    let mut y0 = a.y.round() as i32;
    let x1 = b.x.round() as i32;
    let y1 = b.y.round() as i32;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let r = thick / 2.0;
    loop {
        fill_disk(buf, x0 as f64, y0 as f64, r, c);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn draw_rect_outline(buf: &mut Bgra<'_>, r: Rect, c: [u8; 4], thick: f64) {
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
    draw_line(buf, p0, p1, c, thick);
    draw_line(buf, p1, p2, c, thick);
    draw_line(buf, p2, p3, c, thick);
    draw_line(buf, p3, p0, c, thick);
}

/// Antialiased fill of the *union* of `tris`, one blend per pixel.
/// Sampling the union (rather than each triangle in turn) keeps shared edges seamless.
fn fill_triangles(buf: &mut Bgra<'_>, tris: &[(Point, Point, Point)], color: [u8; 4]) {
    // ponytail: 4x4 box supersampling. Enough for straight edges; go analytic only if 16
    // samples ever show banding on a near-horizontal edge.
    const S: i32 = 4;
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
        return;
    }
    for y in (min.y.floor() as i32)..=(max.y.ceil() as i32) {
        for x in (min.x.floor() as i32)..=(max.x.ceil() as i32) {
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
                buf.blend(x, y, color, hits as f64 / (S * S) as f64);
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

pub fn pixelate(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    bounds: Rect,
    block: i32,
) {
    let mut buf = Bgra {
        pixels,
        width,
        height,
        stride,
    };
    pixelate_buf(&mut buf, bounds, block);
}

fn pixelate_buf(buf: &mut Bgra<'_>, bounds: Rect, block: i32) {
    // Clamp to the buffer: sampling a block whose top-left is outside it would
    // seed the whole block with the transparent black `get` returns for misses.
    let w = buf.width as i32;
    let h = buf.height as i32;
    let x0 = (bounds.x.round() as i32).clamp(0, w);
    let y0 = (bounds.y.round() as i32).clamp(0, h);
    let x1 = ((bounds.x + bounds.width).round() as i32).clamp(0, w);
    let y1 = ((bounds.y + bounds.height).round() as i32).clamp(0, h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let block = block.max(2);
    let mut y = y0;
    while y < y1 {
        let yh = (y + block).min(y1);
        let mut x = x0;
        while x < x1 {
            let xw = (x + block).min(x1);
            let sample = buf.get(x, y);
            for py in y..yh {
                for px in x..xw {
                    buf.put(px, py, sample);
                }
            }
            x += block;
        }
        y += block;
    }
}

fn draw_text(buf: &mut Bgra<'_>, bounds: Rect, text: &str, font_size: f64, c: [u8; 4]) {
    let cell_h = font_size.max(8.0);
    let cell_w = (cell_h * 0.6).max(4.0);
    let mut x = bounds.x;
    let y = bounds.y;
    for ch in text.chars() {
        stamp_glyph(buf, x, y, cell_w, cell_h, ch, c);
        x += cell_w;
        if x > bounds.x + bounds.width {
            break;
        }
    }
}

/// 5×7 bitmap font (ASCII); other glyphs get a filled block so CJK still marks the crop.
fn stamp_glyph(
    buf: &mut Bgra<'_>,
    x: f64,
    y: f64,
    cell_w: f64,
    cell_h: f64,
    ch: char,
    c: [u8; 4],
) {
    let bits = glyph_bits(ch);
    let sx = cell_w / 5.0;
    let sy = cell_h / 7.0;
    for (row, row_bits) in bits.iter().enumerate() {
        for col in 0..5 {
            if *row_bits & (1 << (4 - col)) != 0 {
                let px0 = (x + col as f64 * sx).floor() as i32;
                let py0 = (y + row as f64 * sy).floor() as i32;
                let px1 = (x + (col + 1) as f64 * sx).ceil() as i32;
                let py1 = (y + (row + 1) as f64 * sy).ceil() as i32;
                for py in py0..py1 {
                    for px in px0..px1 {
                        buf.put(px, py, c);
                    }
                }
            }
        }
    }
}

fn glyph_bits(ch: char) -> [u8; 7] {
    match ch {
        'A' | 'a' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'H' | 'h' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' | 'i' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1F],
        _ => {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_punctuation() {
                [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F]
            } else {
                [0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F]
            }
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
    apply_annotations(&mut buf, width, height, stride, origin_x, origin_y, elements);
    crate::codec::encode_png_bgra(&buf, width, height, stride)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::editor::{Element, Shape, DEFAULT_MOSAIC_BLOCK, DEFAULT_STROKE};
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
            stroke: DEFAULT_STROKE,
            stroke_width: 3.0,
            shape: Shape::Rect,
        }
    }

    fn mosaic_el(bounds: Rect) -> Element {
        Element {
            id: 2,
            bounds,
            stroke: DEFAULT_STROKE,
            stroke_width: 1.0,
            shape: Shape::Mosaic {
                block_size: DEFAULT_MOSAIC_BLOCK,
            },
        }
    }

    #[test]
    fn mosaic_makes_interior_blocks_uniform_and_leaves_exterior() {
        let (mut px, stride) = unique(32, 32);
        let original = px.clone();
        apply_annotations(
            &mut px,
            32,
            32,
            stride,
            0.0,
            0.0,
            &[mosaic_el(Rect::new(8.0, 8.0, 16.0, 16.0))],
        );
        for y in 0..32i32 {
            for x in 0..32i32 {
                let i = (y as u32 * stride + x as u32 * 4) as usize;
                let inside = x >= 8 && x < 24 && y >= 8 && y < 24;
                if !inside {
                    assert_eq!(&px[i..i + 4], &original[i..i + 4], "exterior {x},{y}");
                }
            }
        }
        // 16px block size → one cell covering the 16×16 mosaic.
        let sample = &px[(8 * stride + 8 * 4) as usize..][..4];
        for y in 8..24i32 {
            for x in 8..24i32 {
                let i = (y as u32 * stride + x as u32 * 4) as usize;
                assert_eq!(&px[i..i + 4], sample, "interior {x},{y}");
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
            stroke: DEFAULT_STROKE,
            stroke_width: 1.0,
            shape: Shape::Mosaic { block_size: 16 },
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
            .chunks_exact(4)
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
            stroke: DEFAULT_STROKE,
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
        assert!(
            head > neck,
            "head {head} should be wider than neck {neck}"
        );
    }
}

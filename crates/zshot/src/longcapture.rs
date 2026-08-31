//! Long-screenshot stitching, ported from the C# `Zshot.Core.LongCapture`
//! (GrayscaleConvert / VerticalFrameAligner / LongImageBuffer).
//!
//! The C# runner samples a fixed screen region every 450 ms while the user
//! scrolls by hand; here we keep only the pure math: grayscale conversion,
//! SAD-based vertical alignment, and the growing BGRA buffer. No GPU or UI
//! lives in this module so every path is unit-testable off-screen.

/// BGRA → 8-bit luma with the same integer weights as the C# build
/// (`29/150/77` on B/G/R, one rounding-free shift). Used for alignment only —
/// the stitched output stays in BGRA.
pub fn bgra_to_gray(bgra: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut gray = vec![0u8; width * height];
    for (g, px) in gray.iter_mut().zip(bgra.as_chunks::<4>().0) {
        *g = ((px[0] as u32 * 29 + px[1] as u32 * 150 + px[2] as u32 * 77) >> 8) as u8;
    }
    gray
}

/// Outcome of aligning one frame against the previous. `offset_y` counts the
/// overlapping rows to skip at the top of `current`; the new content is
/// `[offset_y..height)`.
pub struct FrameAlignResult {
    pub offset_y: usize,
    pub score: f64,
    pub accepted: bool,
}

impl FrameAlignResult {
    fn rejected() -> Self {
        FrameAlignResult {
            offset_y: 0,
            score: 0.0,
            accepted: false,
        }
    }
}

/// Finds the previous frame's bottom strip inside `current` by SAD over the
/// top `height - strip` candidate rows. Matches the C# `VerticalFrameAligner`
/// exactly, including the clamp order: a frame shorter than the strip falls
/// through `search_max <= 0` rather than shrinking the strip further.
pub fn align_vertical(
    previous: &[u8],
    current: &[u8],
    width: usize,
    height: usize,
    strip_height: usize,
    accept_score: f64,
) -> FrameAlignResult {
    if width == 0
        || height == 0
        || previous.len() < width * height
        || current.len() < width * height
    {
        return FrameAlignResult::rejected();
    }

    let strip = strip_height.clamp(4, (height / 3).max(4));
    if height <= strip {
        return FrameAlignResult::rejected();
    }
    let search_max = height - strip;

    let prev_start = (height - strip) * width;
    let len = strip * width;
    let mut sads = Vec::with_capacity(search_max + 1);

    for y in 0..=search_max {
        let curr_start = y * width;
        let mut sad = 0u64;
        for i in 0..len {
            sad += previous[prev_start + i].abs_diff(current[curr_start + i]) as u64;
        }
        sads.push(sad);
    }

    let best_y = (0..=search_max).min_by_key(|&y| sads[y]).unwrap_or(0);
    let best_sad = sads[best_y];
    // The strip has to be found in exactly one place. Blank or repeating
    // content matches just as well several rows away, and so does a frame with
    // no overlap left after a fast scroll; stitching that guess duplicates or
    // drops rows silently. Offsets within `strip` of the winner are the
    // shoulder of the same match, not a rival one.
    let runner_up = sads
        .iter()
        .enumerate()
        .filter(|(y, _)| y.abs_diff(best_y) > strip)
        .map(|(_, &s)| s)
        .min()
        .unwrap_or(u64::MAX);

    let score = 1.0 - best_sad as f64 / (len * 255) as f64;
    let accepted = score >= accept_score && runner_up > best_sad.saturating_mul(2);
    // best_y is where the previous bottom strip starts in `current`; skip the
    // whole overlap, so the new rows begin at best_y + strip.
    let offset_y = height.min(best_y + strip);
    FrameAlignResult {
        offset_y,
        score,
        accepted,
    }
}

/// Height bookkeeping only — the C# runner checks `TryAppend` before keeping
/// a frame, so the budget decision stays separate from pixel storage.
pub struct LongImageBuffer {
    max_height: usize,
    total_height: usize,
    segment_heights: Vec<usize>,
}

impl Default for LongImageBuffer {
    fn default() -> Self {
        Self::new(16384)
    }
}

impl LongImageBuffer {
    pub fn new(max_height: usize) -> Self {
        LongImageBuffer {
            max_height: max_height.max(1),
            total_height: 0,
            segment_heights: Vec::new(),
        }
    }

    pub fn max_height(&self) -> usize {
        self.max_height
    }

    pub fn total_height(&self) -> usize {
        self.total_height
    }

    pub fn segment_count(&self) -> usize {
        self.segment_heights.len()
    }

    pub fn can_append(&self, height: usize) -> bool {
        height > 0 && self.total_height + height <= self.max_height
    }

    pub fn try_append(&mut self, height: usize) -> bool {
        if !self.can_append(height) {
            return false;
        }
        self.segment_heights.push(height);
        self.total_height += height;
        true
    }
}

/// Growing BGRA pixel buffer for the stitched image. `LongImageBuffer`
/// mirrors the C# height accounting; this type owns the pixels that the
/// accounting describes, so a rejected append leaves both untouched.
pub struct Stitcher {
    width: usize,
    max_height: usize,
    heights: Vec<usize>,
    pixels: Vec<u8>,
}

impl Stitcher {
    /// The first frame is appended whole; exceeding the budget up front means
    /// the region is taller than a D3D texture can hold, so there is nothing
    /// to stitch into.
    pub fn new(width: usize, first_frame_height: usize, max_height: usize) -> Option<Self> {
        let max_height = max_height.max(1);
        if width == 0 || first_frame_height == 0 || first_frame_height > max_height {
            return None;
        }
        let mut s = Stitcher {
            width,
            max_height,
            heights: Vec::new(),
            pixels: Vec::with_capacity(width * 4 * first_frame_height),
        };
        s.heights.push(first_frame_height);
        s.pixels.resize(width * 4 * first_frame_height, 0);
        Some(s)
    }

    /// Writes the first frame's pixels. `new` only reserves and accounts for
    /// the first segment because the caller sizes the buffer before the first
    /// capture lands; the segment count is unchanged.
    pub fn fill_first(&mut self, frame_bgra: &[u8]) -> bool {
        let Some(&rows) = self.heights.first() else {
            return false;
        };
        let bytes = rows * self.width * 4;
        if frame_bgra.len() < bytes {
            return false;
        }
        self.pixels[..bytes].copy_from_slice(&frame_bgra[..bytes]);
        true
    }
    /// Appends the rows `[offset_y..frame_height)` of `frame_bgra`. Rows are
    /// packed (`frame_width * 4` bytes); a width mismatch or an over-budget
    /// append leaves the buffer unchanged.
    pub fn append(
        &mut self,
        frame_bgra: &[u8],
        frame_width: usize,
        frame_height: usize,
        offset_y: usize,
    ) -> bool {
        if frame_width != self.width || frame_bgra.len() < frame_width * 4 * frame_height {
            return false;
        }
        let new_rows = frame_height.saturating_sub(offset_y);
        if new_rows == 0 {
            return false;
        }
        let total = self.total_height();
        if total + new_rows > self.max_height {
            return false;
        }
        let row_bytes = self.width * 4;
        self.pixels
            .extend_from_slice(&frame_bgra[offset_y * row_bytes..frame_height * row_bytes]);
        self.heights.push(new_rows);
        true
    }

    pub fn total_height(&self) -> usize {
        self.heights.iter().sum()
    }

    pub fn segment_count(&self) -> usize {
        self.heights.len()
    }

    /// Hands back (bgra, width, height); stride is always `width * 4`.
    pub fn into_pixels(self) -> (Vec<u8>, u32, u32) {
        let height = self.total_height() as u32;
        (self.pixels, self.width as u32, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_frame(rows: &[&[u8]], width: usize) -> Vec<u8> {
        // Each row is a repeating grayscale value sequence; build w*h bytes.
        let mut out = Vec::with_capacity(rows.len() * width);
        for row in rows {
            for x in 0..width {
                out.push(row[x % row.len()]);
            }
        }
        out
    }

    fn bgra_frame(rows: &[&[u8]], width: usize) -> Vec<u8> {
        // Repeating BGRA quads per row.
        let mut out = Vec::with_capacity(rows.len() * width * 4);
        for row in rows {
            for x in 0..width {
                let o = (x * 4) % row.len();
                out.extend_from_slice(&row[o..o + 4]);
            }
        }
        out
    }

    #[test]
    fn gray_weights_match_csharp() {
        // BGRA order: red is (0,0,255), green (0,255,0), blue (255,0,0).
        assert_eq!(bgra_to_gray(&[0, 0, 255, 255], 1, 1), vec![76]);
        assert_eq!(bgra_to_gray(&[0, 255, 0, 255], 1, 1), vec![149]);
        assert_eq!(bgra_to_gray(&[255, 0, 0, 255], 1, 1), vec![28]);
        assert_eq!(bgra_to_gray(&[255, 255, 255, 255], 1, 1), vec![255]);
        // Alpha is ignored.
        assert_eq!(bgra_to_gray(&[255, 255, 255, 0], 1, 1), vec![255]);
    }

    /// Deterministic byte stream, one distinct row at a time: alignment needs a
    /// unique SAD minimum, and repeating rows would match at every period.
    fn lcg_rows(height: usize, width: usize, seed: u32) -> Vec<Vec<u8>> {
        let mut state = seed;
        let mut rows = Vec::with_capacity(height);
        for _ in 0..height {
            rows.push(
                (0..width)
                    .map(|_| {
                        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                        (state >> 24) as u8
                    })
                    .collect(),
            );
        }
        rows
    }

    #[test]
    fn identical_frames_have_no_new_content() {
        let rows = lcg_rows(32, 8, 0x12345678);
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let frame = gray_frame(&refs, 8);
        let r = align_vertical(&frame, &frame, 8, 32, 8, 0.35);
        assert!(r.accepted, "score {}", r.score);
        assert_eq!(r.offset_y, 32);
        assert!((r.score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn scrolled_by_k_appends_k_rows() {
        let width = 8;
        let height = 32;
        let strip = 8;
        let k = 5;
        // `previous` is a deterministic pseudo-random pattern per row so SAD
        // has a unique minimum; `current` is a full frame: `previous` scrolled
        // down by k with k fresh rows entering at the bottom.
        let prev_rows = lcg_rows(height, width, 0x12345678);
        let new_rows = lcg_rows(k, width, 0xABCDEF01);
        let prev_refs: Vec<&[u8]> = prev_rows.iter().map(Vec::as_slice).collect();
        let curr_rows: Vec<&[u8]> = prev_refs[k..]
            .iter()
            .copied()
            .chain(new_rows.iter().map(Vec::as_slice))
            .collect();
        let prev = gray_frame(&prev_refs, width);
        let curr = gray_frame(&curr_rows, width);
        let r = align_vertical(&prev, &curr, width, height, strip, 0.35);
        assert!(r.accepted, "score {}", r.score);
        assert_eq!(r.offset_y, height - k);
    }

    #[test]
    fn unrelated_frames_are_rejected() {
        // Independent noise scores ~2/3 (E|a-b| = 85/255), which is why the
        // caller passes 0.9: a wrong window swapped in mid-capture must not
        // clear the bar on score alone.
        let width = 8;
        let height = 32;
        let a_rows = lcg_rows(height, width, 7);
        let b_rows = lcg_rows(height, width, 0xDEADBEEF);
        let a = gray_frame(&a_rows.iter().map(Vec::as_slice).collect::<Vec<_>>(), width);
        let b = gray_frame(&b_rows.iter().map(Vec::as_slice).collect::<Vec<_>>(), width);
        let r = align_vertical(&a, &b, width, height, 8, 0.9);
        assert!(!r.accepted, "score {}", r.score);
    }

    #[test]
    fn ambiguous_matches_are_rejected() {
        // A blank page: the previous bottom strip matches everywhere, so the
        // SAD score is a perfect 1.0 while the offset is a coin flip. Scoring
        // alone would accept this and append a screen of blank rows.
        let frame = vec![255u8; 8 * 32];
        let r = align_vertical(&frame, &frame, 8, 32, 8, 0.35);
        assert!(!r.accepted, "score {}", r.score);
    }

    #[test]
    fn tiny_frames_cannot_align() {
        // height 4 clamps strip to 4 → search_max == 0.
        let frame = vec![128u8; 4 * 4];
        let r = align_vertical(&frame, &frame, 4, 4, 8, 0.35);
        assert!(!r.accepted);
        assert_eq!(r.offset_y, 0);
        // Empty inputs are rejected, not a panic.
        let r = align_vertical(&[], &[], 0, 0, 8, 0.35);
        assert!(!r.accepted);
    }

    #[test]
    fn buffer_stops_at_max_height() {
        let mut buf = LongImageBuffer::new(20);
        assert!(buf.try_append(10));
        assert!(buf.try_append(10));
        assert!(!buf.can_append(1));
        assert!(!buf.try_append(1));
        assert_eq!(buf.total_height(), 20);
        assert_eq!(buf.segment_count(), 2);
        assert!(!buf.try_append(0));
    }

    #[test]
    fn stitcher_appends_only_new_rows() {
        let width = 2;
        let row0: &[u8] = &[1, 2, 3, 4];
        let row1: &[u8] = &[5, 6, 7, 8];
        let mut s = Stitcher::new(width, 4, 16).unwrap();
        // The caller writes the first frame's crop into the buffer; here the
        // interesting part is what `append` adds after it.
        // Second frame scrolled by 3: only its last row is new.
        let row2: &[u8] = &[9, 10, 11, 12];
        let second = bgra_frame(&[row1, row0, row1, row2], width);
        assert!(s.append(&second, width, 4, 3));
        assert_eq!(s.total_height(), 5);
        assert_eq!(s.segment_count(), 2);
        let (out, ow, oh) = s.into_pixels();
        assert_eq!((ow, oh), (2, 5));
        // First frame is zero-initialized until the caller blits the crop, so
        // only the appended row carries the second frame's bytes.
        assert_eq!(&out[4 * 8..], &bgra_frame(&[row2], width)[..]);
    }

    #[test]
    fn fill_first_writes_the_reserved_rows() {
        let mut s = Stitcher::new(2, 2, 8).unwrap();
        let frame = [9u8; 2 * 2 * 4];
        assert!(s.fill_first(&frame));
        assert_eq!(s.segment_count(), 1);
        let (out, _, _) = s.into_pixels();
        assert_eq!(out, frame.to_vec());
        // Short input is rejected without touching the buffer.
        let mut s = Stitcher::new(2, 2, 8).unwrap();
        assert!(!s.fill_first(&[1u8; 3]));
        let (out, _, _) = s.into_pixels();
        assert_eq!(out, vec![0u8; 2 * 2 * 4]);
    }

    #[test]
    fn stitcher_rejects_over_budget_and_bad_width() {
        let width = 2;
        let frame = bgra_frame(&[&[0u8, 0, 0, 0], &[0u8, 0, 0, 0]], width);
        let mut s = Stitcher::new(width, 2, 4).unwrap();
        // 2 + 3 > 4.
        let three_rows = bgra_frame(&[&[0u8, 0, 0, 0][..]; 3], width);
        assert!(!s.append(&three_rows, width, 3, 0));
        assert_eq!(s.total_height(), 2);
        assert_eq!(s.segment_count(), 1);
        // Wrong width must not touch state either.
        assert!(!s.append(&frame, 3, 2, 0));
        assert_eq!(s.total_height(), 2);
        // First frame taller than the budget yields no stitcher at all.
        assert!(Stitcher::new(width, 5, 4).is_none());
    }
}

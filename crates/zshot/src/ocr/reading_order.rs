//! Group detected boxes into reading order. Ported from C# `OcrReadingOrder`.
//!
//! The detector emits boxes in score order, so a raw join interleaves columns
//! and reads as noise. Grouping by vertical band first is what makes multi-line
//! Chinese come out as prose.

/// One detected box: the recognized text plus its detection polygon.
pub struct Block {
    pub text: String,
    pub poly: Vec<(f64, f64)>,
}

impl Block {
    fn centroid(&self) -> (f64, f64) {
        if self.poly.is_empty() {
            return (0.0, 0.0);
        }
        let n = self.poly.len() as f64;
        let (sx, sy) = self
            .poly
            .iter()
            .fold((0.0, 0.0), |(ax, ay), (x, y)| (ax + x, ay + y));
        (sx / n, sy / n)
    }

    fn height(&self) -> f64 {
        let ys = || self.poly.iter().map(|(_, y)| *y);
        match (
            ys().fold(f64::NEG_INFINITY, f64::max),
            ys().fold(f64::INFINITY, f64::min),
        ) {
            (max, min) if max.is_finite() && min.is_finite() => max - min,
            _ => 0.0,
        }
    }
}

pub fn to_plain_text(blocks: &[Block]) -> String {
    group_lines(blocks)
        .iter()
        .map(|line| {
            line.iter()
                .map(|b| b.text.as_str())
                .filter(|t| !t.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn group_lines(blocks: &[Block]) -> Vec<Vec<&Block>> {
    match blocks.len() {
        0 => return Vec::new(),
        1 => return vec![vec![&blocks[0]]],
        _ => {}
    }

    // 8px floor keeps tiny detections from each becoming their own line.
    let threshold = (median_height(blocks) * 0.6).max(8.0);
    let mut ordered: Vec<&Block> = blocks.iter().collect();
    ordered.sort_by(|a, b| {
        let (ax, ay) = a.centroid();
        let (bx, by) = b.centroid();
        ay.total_cmp(&by).then(ax.total_cmp(&bx))
    });

    let mut lines: Vec<Vec<&Block>> = Vec::new();
    for block in ordered {
        // Compare against the running mean of the open line, not its first box,
        // so a slightly rising row of boxes stays one line.
        let same_line = lines.last().is_some_and(|line| {
            let mean = line.iter().map(|b| b.centroid().1).sum::<f64>() / line.len() as f64;
            (block.centroid().1 - mean).abs() <= threshold
        });
        if same_line {
            lines.last_mut().expect("checked above").push(block);
        } else {
            lines.push(vec![block]);
        }
    }

    for line in &mut lines {
        line.sort_by(|a, b| a.centroid().0.total_cmp(&b.centroid().0));
    }
    lines
}

fn median_height(blocks: &[Block]) -> f64 {
    let mut heights: Vec<f64> = blocks
        .iter()
        .map(Block::height)
        .filter(|h| *h > 0.0)
        .collect();
    if heights.is_empty() {
        return 16.0;
    }
    heights.sort_by(f64::total_cmp);
    heights[heights.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned box, the shape the detector emits for horizontal text.
    fn block(text: &str, x: f64, y: f64, w: f64, h: f64) -> Block {
        Block {
            text: text.into(),
            poly: vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
        }
    }

    #[test]
    fn empty_and_single_blocks() {
        assert_eq!(to_plain_text(&[]), "");
        assert_eq!(
            to_plain_text(&[block("只有一行", 0.0, 0.0, 100.0, 20.0)]),
            "只有一行"
        );
    }

    #[test]
    fn boxes_on_one_row_join_with_a_space_left_to_right() {
        // Fed in right-to-left to prove the sort runs.
        let blocks = vec![
            block("World", 120.0, 10.0, 80.0, 20.0),
            block("Hello", 10.0, 12.0, 80.0, 20.0),
        ];
        assert_eq!(to_plain_text(&blocks), "Hello World");
    }

    #[test]
    fn rows_further_apart_than_the_threshold_become_separate_lines() {
        let blocks = vec![
            block("second", 10.0, 100.0, 80.0, 20.0),
            block("first", 10.0, 10.0, 80.0, 20.0),
        ];
        assert_eq!(to_plain_text(&blocks), "first\nsecond");
    }

    #[test]
    fn a_slightly_ragged_row_stays_one_line() {
        // 20px tall boxes -> threshold 12; each step is under it.
        let blocks = vec![
            block("a", 10.0, 10.0, 20.0, 20.0),
            block("b", 40.0, 14.0, 20.0, 20.0),
            block("c", 70.0, 18.0, 20.0, 20.0),
        ];
        assert_eq!(to_plain_text(&blocks), "a b c");
    }

    #[test]
    fn blank_text_is_dropped_but_keeps_its_line() {
        let blocks = vec![
            block("  ", 10.0, 10.0, 20.0, 20.0),
            block("kept", 40.0, 10.0, 20.0, 20.0),
        ];
        assert_eq!(to_plain_text(&blocks), "kept");
    }

    #[test]
    fn degenerate_polygons_do_not_panic() {
        let blocks = vec![
            Block {
                text: "no poly".into(),
                poly: Vec::new(),
            },
            block("real", 10.0, 200.0, 20.0, 20.0),
        ];
        assert_eq!(to_plain_text(&blocks), "no poly\nreal");
    }
}

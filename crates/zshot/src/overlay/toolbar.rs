//! Overlay toolbar catalog and placement. Ported from C# OverlayToolbarCatalog / ToolbarAnchor.

use crate::geom::{Point, Rect};

pub const COLOR: &str = "color";
pub const SELECT: &str = "select";
pub const RECT: &str = "rect";
pub const ELLIPSE: &str = "ellipse";
pub const LINE: &str = "line";
pub const ARROW: &str = "arrow";
pub const PEN: &str = "pen";
pub const TEXT: &str = "text";
pub const MOSAIC: &str = "mosaic";
pub const NUMBER: &str = "number";
pub const UNDO: &str = "undo";
pub const REDO: &str = "redo";
pub const CLEAR: &str = "clear";
pub const OCR: &str = "ocr";
pub const TRANSLATE: &str = "translate";
pub const LONG_CAPTURE: &str = "longCapture";
pub const SAVE: &str = "save";
pub const COMPLETE: &str = "complete";
pub const CANCEL: &str = "cancel";
pub const COPY: &str = "copy";

pub const CUSTOMIZABLE: &[&str] = &[
    COLOR, SELECT, RECT, ELLIPSE, LINE, ARROW, PEN, TEXT, MOSAIC, NUMBER, UNDO, REDO, CLEAR, OCR,
    TRANSLATE, LONG_CAPTURE, SAVE,
];

#[allow(dead_code)]
pub const DEFAULT_HIDDEN: &[&str] = &[ELLIPSE, TEXT, NUMBER, REDO];

pub const PHASE1_ITEMS: &[&str] = &[
    SELECT, RECT, ARROW, PEN, MOSAIC, TEXT, UNDO, REDO, CLEAR, OCR, TRANSLATE, SAVE, COMPLETE,
    CANCEL,
];

pub const BTN: f64 = 36.0;
pub const ICON: f32 = 20.0;
pub const PAD: f64 = 4.0;
pub const ITEM_GAP: f64 = 2.0;
pub const H: f64 = 44.0;
pub const ANCHOR_GAP: f64 = 8.0;

pub fn bar_size(n: usize) -> (f64, f64) {
    let n = n as f64;
    let w = PAD * 2.0 + n * BTN + (n - 1.0).max(0.0) * ITEM_GAP;
    (w, H)
}

pub fn icon_path(id: &str) -> Option<&'static str> {
    Some(match id {
        SELECT => "toolbar/select.svg",
        RECT => "toolbar/rect.svg",
        ARROW => "toolbar/arrow.svg",
        PEN => "toolbar/pen.svg",
        MOSAIC => "toolbar/mosaic.svg",
        TEXT => "toolbar/text.svg",
        UNDO => "toolbar/undo.svg",
        REDO => "toolbar/redo.svg",
        CLEAR => "toolbar/clear.svg",
        OCR => "toolbar/ocr.svg",
        TRANSLATE => "toolbar/translate.svg",
        SAVE => "toolbar/save.svg",
        COMPLETE => "toolbar/complete.svg",
        CANCEL => "toolbar/cancel.svg",
        _ => return None,
    })
}

/// Hover label. Every `PHASE1_ITEMS` entry needs one; the rest are unreachable
/// until they ship, so they fall back to the id.
pub fn label(id: &str) -> &'static str {
    match id {
        SELECT => "选择",
        RECT => "矩形",
        ARROW => "箭头",
        PEN => "画笔",
        MOSAIC => "马赛克",
        TEXT => "文字",
        UNDO => "撤销",
        REDO => "重做",
        CLEAR => "清空",
        OCR => "取字",
        TRANSLATE => "翻译",
        SAVE => "保存",
        COMPLETE => "完成",
        CANCEL => "取消",
        _ => "",
    }
}

pub fn parse_hidden(stored: Option<&str>) -> Vec<String> {
    match stored {
        None => DEFAULT_HIDDEN.iter().map(|s| (*s).to_string()).collect(),
        Some(s) => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty() && CUSTOMIZABLE.contains(p))
            .map(str::to_string)
            .collect(),
    }
}

pub fn is_visible(id: &str, hidden: &[String]) -> bool {
    if id == COPY {
        return false;
    }
    if id == COMPLETE || id == CANCEL {
        return true;
    }
    !CUSTOMIZABLE.contains(&id) || !hidden.iter().any(|h| h == id)
}

/// Prefer below the selection; flip above if it would leave `monitor`; clamp inside.
pub fn place_toolbar(
    selection: Rect,
    toolbar_width: f64,
    toolbar_height: f64,
    monitor: Rect,
    gap: f64,
) -> Point {
    let mut x = selection.x + selection.width / 2.0 - toolbar_width / 2.0;
    let below = selection.y + selection.height + gap;
    let above = selection.y - gap - toolbar_height;
    let mut y = if below + toolbar_height <= monitor.y + monitor.height {
        below
    } else {
        above
    };
    let max_x = (monitor.x + monitor.width - toolbar_width).max(monitor.x);
    let max_y = (monitor.y + monitor.height - toolbar_height).max(monitor.y);
    x = x.clamp(monitor.x, max_x);
    y = y.clamp(monitor.y, max_y);
    Point { x, y }
}

/// Where the OCR/translate panel goes relative to the toolbar.
pub struct PanelPlacement {
    pub x: f64,
    /// False means the caller must bottom-anchor at `bar.y - gap`: the content
    /// height is unknown before layout, so the top edge cannot be precomputed.
    pub anchor_below: bool,
    pub body_max_h: f64,
}

/// The toolbar is clamped to the monitor by its own (narrower) width; the wider
/// panel needs its own clamp. Vertically it prefers below the toolbar and flips
/// above when that side has more room. The body scrolls, so shrinking max_h to
/// fit the monitor loses no text.
pub fn place_panel(
    bar: Rect,
    panel_w: f64,
    chrome: f64,
    body_limit: (f64, f64),
    monitor: Rect,
    gap: f64,
    margin: f64,
) -> PanelPlacement {
    let mon_l = monitor.x + margin;
    let mon_t = monitor.y + margin;
    let mon_r = monitor.x + monitor.width - margin;
    let mon_b = monitor.y + monitor.height - margin;
    let x = bar.x.clamp(mon_l, (mon_r - panel_w).max(mon_l));
    let below = mon_b - (bar.y + bar.height + gap);
    let above = bar.y - gap - mon_t;
    let anchor_below = below >= above;
    let avail = if anchor_below { below } else { above };
    let body_max_h = (avail - chrome).clamp(body_limit.0, body_limit.1);
    PanelPlacement {
        x,
        anchor_below,
        body_max_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_and_cancel_are_always_visible() {
        let hidden = parse_hidden(Some("complete,cancel,save"));
        assert!(is_visible(COMPLETE, &hidden));
        assert!(is_visible(CANCEL, &hidden));
        assert!(!is_visible(SAVE, &hidden));
    }

    #[test]
    fn copy_is_never_on_the_bar() {
        assert!(!is_visible(COPY, &parse_hidden(None)));
        assert!(!is_visible(COPY, &parse_hidden(Some(""))));
    }

    #[test]
    fn default_hidden_matches_csharp() {
        let hidden = parse_hidden(None);
        assert!(!is_visible(ELLIPSE, &hidden));
        assert!(!is_visible(TEXT, &hidden));
        assert!(!is_visible(NUMBER, &hidden));
        assert!(!is_visible(REDO, &hidden));
        assert!(is_visible(COLOR, &hidden));
        assert!(is_visible(SELECT, &hidden));
        assert!(is_visible(RECT, &hidden));
        assert!(is_visible(UNDO, &hidden));
        assert!(is_visible(SAVE, &hidden));
    }

    #[test]
    fn toolbar_prefers_below_selection() {
        let pos = place_toolbar(
            Rect::new(100.0, 80.0, 200.0, 120.0),
            200.0,
            40.0,
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            8.0,
        );
        assert_eq!(pos.x, 100.0);
        assert_eq!(pos.y, 208.0);
    }

    #[test]
    fn toolbar_flips_above_when_below_does_not_fit() {
        let pos = place_toolbar(
            Rect::new(100.0, 1000.0, 200.0, 60.0),
            400.0,
            40.0,
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            8.0,
        );
        assert_eq!(pos.y, 1000.0 - 8.0 - 40.0);
    }

    #[test]
    fn toolbar_clamps_into_monitor() {
        let pos = place_toolbar(
            Rect::new(10.0, 10.0, 40.0, 20.0),
            400.0,
            40.0,
            Rect::new(0.0, 0.0, 500.0, 80.0),
            8.0,
        );
        assert!(pos.x >= 0.0);
        assert!(pos.x + 400.0 <= 500.0);
        assert!(pos.y >= 0.0);
        assert!(pos.y + 40.0 <= 80.0);
    }

    #[test]
    fn panel_prefers_below_toolbar() {
        let p = place_panel(
            Rect::new(100.0, 208.0, 462.0, 44.0),
            320.0,
            80.0,
            (48.0, 260.0),
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            8.0,
            4.0,
        );
        assert!(p.anchor_below);
        assert_eq!(p.x, 100.0);
        assert_eq!(p.body_max_h, 260.0);
    }

    #[test]
    fn panel_flips_above_when_toolbar_sits_at_monitor_bottom() {
        // Selection spans nearly the full monitor: the toolbar clamps to the
        // bottom edge, and the panel would start off-screen without the flip.
        let p = place_panel(
            Rect::new(100.0, 1036.0, 462.0, 44.0),
            320.0,
            80.0,
            (48.0, 260.0),
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            8.0,
            4.0,
        );
        assert!(!p.anchor_below);
        assert_eq!(p.body_max_h, 260.0);
    }

    #[test]
    fn panel_shrinks_body_to_fit() {
        // Short monitor: above the bar is 182 - 8 - 4 = 170 px, below is
        // 396 - 234 = 162 px. Above wins and the body gets 170 - 80 = 90 px.
        let p = place_panel(
            Rect::new(100.0, 182.0, 462.0, 44.0),
            320.0,
            80.0,
            (48.0, 260.0),
            Rect::new(0.0, 0.0, 1920.0, 400.0),
            8.0,
            4.0,
        );
        assert!(!p.anchor_below);
        assert_eq!(p.body_max_h, 90.0);
    }

    #[test]
    fn panel_clamps_into_monitor() {
        // A heavily hidden toolbar can be far narrower than the 320 px panel
        // (2 visible buttons = 82 px). Clamped to the monitor's right edge, its
        // x would push the panel off-screen; the panel slides left on its own.
        let p = place_panel(
            Rect::new(1218.0, 208.0, 82.0, 44.0),
            320.0,
            80.0,
            (48.0, 260.0),
            Rect::new(800.0, 0.0, 500.0, 1080.0),
            8.0,
            4.0,
        );
        assert_eq!(p.x, 976.0);
        assert!(p.x >= 804.0);
        assert!(p.x + 320.0 <= 1296.0);
    }

    #[test]
    fn bar_size_matches_phase1_layout() {
        assert_eq!(bar_size(12), (462.0, 44.0));
        assert_eq!(bar_size(1), (44.0, 44.0));
        assert_eq!(bar_size(0), (8.0, 44.0));
    }

    #[test]
    fn every_phase1_item_has_an_icon() {
        for id in PHASE1_ITEMS {
            assert!(icon_path(id).is_some(), "{id}");
        }
    }

    #[test]
    fn every_phase1_item_has_a_label() {
        for id in PHASE1_ITEMS {
            assert!(!label(id).is_empty(), "{id}");
        }
    }
}

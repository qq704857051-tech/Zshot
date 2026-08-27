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
    SELECT, RECT, ARROW, PEN, MOSAIC, TEXT, UNDO, REDO, CLEAR, SAVE, COMPLETE, CANCEL,
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

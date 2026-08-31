//! Selection hit-test / move / resize. Ported from C# SelectionInteraction.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }

    pub fn normalized(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let x = x0.min(x1);
        let y = y0.min(y1);
        Self::new(x, y, (x0 - x1).abs(), (y0 - y1).abs())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionHitKind {
    None,
    Inside,
    North,
    South,
    East,
    West,
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

fn handle(cx: f64, cy: f64, half: f64) -> Rect {
    Rect::new(cx - half, cy - half, half * 2.0, half * 2.0)
}

pub fn hit_test(selection: Rect, point: Point, handle_size: f64) -> SelectionHitKind {
    let half = handle_size.max(2.0) / 2.0;
    if handle(selection.x, selection.y, half).contains(point) {
        return SelectionHitKind::NorthWest;
    }
    if handle(selection.right(), selection.y, half).contains(point) {
        return SelectionHitKind::NorthEast;
    }
    if handle(selection.x, selection.bottom(), half).contains(point) {
        return SelectionHitKind::SouthWest;
    }
    if handle(selection.right(), selection.bottom(), half).contains(point) {
        return SelectionHitKind::SouthEast;
    }
    if handle(selection.x + selection.width / 2.0, selection.y, half).contains(point) {
        return SelectionHitKind::North;
    }
    if handle(
        selection.x + selection.width / 2.0,
        selection.bottom(),
        half,
    )
    .contains(point)
    {
        return SelectionHitKind::South;
    }
    if handle(selection.x, selection.y + selection.height / 2.0, half).contains(point) {
        return SelectionHitKind::West;
    }
    if handle(
        selection.right(),
        selection.y + selection.height / 2.0,
        half,
    )
    .contains(point)
    {
        return SelectionHitKind::East;
    }
    if selection.contains(point) {
        return SelectionHitKind::Inside;
    }
    SelectionHitKind::None
}

pub fn move_rect(selection: Rect, dx: f64, dy: f64, bounds: Rect) -> Rect {
    let x = (selection.x + dx).clamp(bounds.x, bounds.x + bounds.width - selection.width);
    let y = (selection.y + dy).clamp(bounds.y, bounds.y + bounds.height - selection.height);
    Rect { x, y, ..selection }
}

pub fn resize_rect(
    selection: Rect,
    hit: SelectionHitKind,
    pointer: Point,
    bounds: Rect,
    min_size: f64,
) -> Rect {
    let mut left = selection.x;
    let mut top = selection.y;
    let mut right = selection.right();
    let mut bottom = selection.bottom();
    let min = min_size.max(1.0);

    match hit {
        SelectionHitKind::West | SelectionHitKind::NorthWest | SelectionHitKind::SouthWest => {
            left = pointer.x.clamp(bounds.x, right - min);
        }
        _ => {}
    }
    match hit {
        SelectionHitKind::East | SelectionHitKind::NorthEast | SelectionHitKind::SouthEast => {
            right = pointer.x.clamp(left + min, bounds.x + bounds.width);
        }
        _ => {}
    }
    match hit {
        SelectionHitKind::North | SelectionHitKind::NorthWest | SelectionHitKind::NorthEast => {
            top = pointer.y.clamp(bounds.y, bottom - min);
        }
        _ => {}
    }
    match hit {
        SelectionHitKind::South | SelectionHitKind::SouthWest | SelectionHitKind::SouthEast => {
            bottom = pointer.y.clamp(top + min, bounds.y + bounds.height);
        }
        _ => {}
    }

    Rect::new(left, top, right - left, bottom - top)
}

/// Physical crop from a DIP/canvas rect. Drag selections include both endpoints (+1).
pub fn physical_source_rect(
    dip: Rect,
    from_drag: bool,
    canvas_w: f64,
    canvas_h: f64,
    phys_w: i32,
    phys_h: i32,
) -> (i32, i32, i32, i32) {
    let ratio_x = phys_w as f64 / canvas_w.max(1.0);
    let ratio_y = phys_h as f64 / canvas_h.max(1.0);
    let x1 = (dip.x * ratio_x).round() as i32;
    let y1 = (dip.y * ratio_y).round() as i32;
    let x2 = ((dip.x + dip.width) * ratio_x).round() as i32;
    let y2 = ((dip.y + dip.height) * ratio_y).round() as i32;
    let x = x1.min(x2).max(0);
    let y = y1.min(y2).max(0);
    let extra = if from_drag { 1 } else { 0 };
    let mut w = (x2 - x1).abs() + extra;
    let mut h = (y2 - y1).abs() + extra;
    w = w.max(0).min(phys_w - x).max(0);
    h = h.max(0).min(phys_h - y).max(0);
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_corners_and_inside() {
        let r = Rect::new(10.0, 20.0, 100.0, 80.0);
        assert_eq!(
            hit_test(r, Point { x: 10.0, y: 20.0 }, 8.0),
            SelectionHitKind::NorthWest
        );
        assert_eq!(
            hit_test(r, Point { x: 110.0, y: 100.0 }, 8.0),
            SelectionHitKind::SouthEast
        );
        assert_eq!(
            hit_test(r, Point { x: 50.0, y: 50.0 }, 8.0),
            SelectionHitKind::Inside
        );
        assert_eq!(
            hit_test(r, Point { x: 0.0, y: 0.0 }, 8.0),
            SelectionHitKind::None
        );
    }

    #[test]
    fn move_clamps_to_bounds() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        let b = Rect::new(0.0, 0.0, 100.0, 100.0);
        let moved = move_rect(r, 1000.0, -1000.0, b);
        assert_eq!(moved.x, 80.0);
        assert_eq!(moved.y, 0.0);
    }

    #[test]
    fn resize_east_respects_min() {
        let r = Rect::new(10.0, 10.0, 40.0, 40.0);
        let b = Rect::new(0.0, 0.0, 200.0, 200.0);
        let out = resize_rect(
            r,
            SelectionHitKind::East,
            Point { x: 12.0, y: 30.0 },
            b,
            8.0,
        );
        assert!((out.width - 8.0).abs() < 1e-9);
        assert!((out.x - 10.0).abs() < 1e-9);
    }

    #[test]
    fn drag_physical_rect_includes_endpoint() {
        let dip = Rect::new(0.0, 0.0, 10.0, 10.0);
        let (x, y, w, h) = physical_source_rect(dip, true, 10.0, 10.0, 10, 10);
        assert_eq!((x, y, w, h), (0, 0, 10, 10));
    }
}

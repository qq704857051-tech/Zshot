//! Annotation document, history, and pointer FSM.
//! Ported from C# EditorHistory / OverlayAnnotationController.

use crate::geom::{Point, Rect};

pub const DEFAULT_STROKE: u32 = super::theme::STROKE_ARGB;
pub const DEFAULT_STROKE_WIDTH: f64 = 4.0;
pub const DEFAULT_MOSAIC_BLOCK: i32 = 16;
const MIN_KEEP: f64 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect,
    Arrow {
        start: Point,
        end: Point,
        head: f64,
    },
    Pen {
        points: Vec<Point>,
    },
    Mosaic {
        block_size: i32,
    },
    Text {
        text: String,
        font_size: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub id: u64,
    pub bounds: Rect,
    pub stroke: u32,
    pub stroke_width: f64,
    pub shape: Shape,
}

impl Element {
    fn move_by(&mut self, dx: f64, dy: f64) {
        self.bounds.x += dx;
        self.bounds.y += dy;
        match &mut self.shape {
            Shape::Arrow { start, end, .. } => {
                start.x += dx;
                start.y += dy;
                end.x += dx;
                end.y += dy;
            }
            Shape::Pen { points } => {
                for p in points {
                    p.x += dx;
                    p.y += dy;
                }
            }
            _ => {}
        }
    }

    fn is_pen(&self) -> bool {
        matches!(self.shape, Shape::Pen { .. })
    }

    pub fn hit_test(&self, p: Point) -> bool {
        match &self.shape {
            Shape::Arrow { start, end, .. } => {
                dist_to_segment(p, *start, *end) <= self.stroke_width.max(4.0)
            }
            Shape::Pen { points } => {
                if points.is_empty() {
                    return false;
                }
                if points.len() == 1 {
                    return dist_to_segment(p, points[0], points[0]) <= self.stroke_width.max(4.0);
                }
                points.windows(2).any(|w| {
                    dist_to_segment(p, w[0], w[1]) <= self.stroke_width.max(4.0)
                })
            }
            _ => self.bounds.contains(p),
        }
    }
}

fn dist_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let vx = b.x - a.x;
    let vy = b.y - a.y;
    let len2 = vx * vx + vy * vy;
    if len2 <= f64::EPSILON {
        let dx = p.x - a.x;
        let dy = p.y - a.y;
        return (dx * dx + dy * dy).sqrt();
    }
    let t = ((p.x - a.x) * vx + (p.y - a.y) * vy) / len2;
    let t = t.clamp(0.0, 1.0);
    let qx = a.x + t * vx;
    let qy = a.y + t * vy;
    let ex = p.x - qx;
    let ey = p.y - qy;
    (ex * ex + ey * ey).sqrt()
}

#[derive(Clone, Debug)]
enum Command {
    Add(Element),
    Clear { snapshot: Vec<Element> },
    Move { id: u64, dx: f64, dy: f64 },
}

#[derive(Debug)]
pub struct History {
    undo: Vec<Command>,
    redo: Vec<Command>,
    max_depth: usize,
}

impl History {
    pub fn new(max_depth: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_depth: max_depth.max(1),
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn execute(&mut self, doc: &mut Vec<Element>, cmd: Command) {
        apply(doc, &cmd, false);
        self.undo.push(cmd);
        if self.undo.len() > self.max_depth {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, doc: &mut Vec<Element>) -> bool {
        let Some(cmd) = self.undo.pop() else {
            return false;
        };
        apply(doc, &cmd, true);
        self.redo.push(cmd);
        true
    }

    pub fn redo(&mut self, doc: &mut Vec<Element>) -> bool {
        let Some(cmd) = self.redo.pop() else {
            return false;
        };
        apply(doc, &cmd, false);
        self.undo.push(cmd);
        true
    }
}

fn apply(doc: &mut Vec<Element>, cmd: &Command, undo: bool) {
    match cmd {
        Command::Add(el) => {
            if undo {
                doc.retain(|e| e.id != el.id);
            } else if !doc.iter().any(|e| e.id == el.id) {
                doc.push(el.clone());
            }
        }
        Command::Clear { snapshot } => {
            if undo {
                *doc = snapshot.clone();
            } else {
                doc.clear();
            }
        }
        Command::Move { id, dx, dy } => {
            let (dx, dy) = if undo { (-dx, -dy) } else { (*dx, *dy) };
            if let Some(el) = doc.iter_mut().find(|e| e.id == *id) {
                el.move_by(dx, dy);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Rect,
    Arrow,
    Pen,
    Mosaic,
    Text,
}

impl Tool {
    pub fn id(self) -> &'static str {
        match self {
            Tool::Select => super::toolbar::SELECT,
            Tool::Rect => super::toolbar::RECT,
            Tool::Arrow => super::toolbar::ARROW,
            Tool::Pen => super::toolbar::PEN,
            Tool::Mosaic => super::toolbar::MOSAIC,
            Tool::Text => super::toolbar::TEXT,
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            super::toolbar::SELECT => Tool::Select,
            super::toolbar::RECT => Tool::Rect,
            super::toolbar::ARROW => Tool::Arrow,
            super::toolbar::PEN => Tool::Pen,
            super::toolbar::MOSAIC => Tool::Mosaic,
            super::toolbar::TEXT => Tool::Text,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressKind {
    Capture,
    RequestText,
}

#[derive(Debug)]
pub struct Session {
    pub elements: Vec<Element>,
    pub history: History,
    pub tool: Tool,
    pub draft: Option<Element>,
    pub selected_id: Option<u64>,
    next_id: u64,
    press: Point,
    dragging: bool,
    move_dx: f64,
    move_dy: f64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
            history: History::new(80),
            tool: Tool::Select,
            draft: None,
            selected_id: None,
            next_id: 1,
            press: Point { x: 0.0, y: 0.0 },
            dragging: false,
            move_dx: 0.0,
            move_dy: 0.0,
        }
    }

    pub fn visible(&self) -> impl Iterator<Item = &Element> {
        self.elements.iter().chain(self.draft.as_ref())
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn pointer_pressed(&mut self, pt: Point) -> PressKind {
        self.press = pt;
        self.dragging = true;
        self.move_dx = 0.0;
        self.move_dy = 0.0;
        if self.tool == Tool::Select {
            self.selected_id = self
                .elements
                .iter()
                .rev()
                .find(|e| e.hit_test(pt))
                .map(|e| e.id);
            return PressKind::Capture;
        }
        if self.tool == Tool::Text {
            self.dragging = false;
            return PressKind::RequestText;
        }
        self.draft = Some(self.create_draft(pt));
        PressKind::Capture
    }

    pub fn pointer_moved(&mut self, pt: Point) {
        if !self.dragging {
            return;
        }
        if self.tool == Tool::Select {
            if let Some(id) = self.selected_id {
                let dx = pt.x - self.press.x;
                let dy = pt.y - self.press.y;
                if let Some(el) = self.elements.iter_mut().find(|e| e.id == id) {
                    el.move_by(dx, dy);
                }
                self.move_dx += dx;
                self.move_dy += dy;
                self.press = pt;
            }
            return;
        }
        if let Some(draft) = self.draft.as_mut() {
            update_draft(draft, self.press, pt);
        }
    }

    pub fn pointer_released(&mut self) {
        if !self.dragging {
            return;
        }
        self.dragging = false;
        if self.tool == Tool::Select {
            if let Some(id) = self.selected_id {
                if self.move_dx != 0.0 || self.move_dy != 0.0 {
                    if let Some(el) = self.elements.iter_mut().find(|e| e.id == id) {
                        el.move_by(-self.move_dx, -self.move_dy);
                    }
                    self.history.execute(
                        &mut self.elements,
                        Command::Move {
                            id,
                            dx: self.move_dx,
                            dy: self.move_dy,
                        },
                    );
                }
            }
        }
        if let Some(draft) = self.draft.take() {
            let tiny = draft.bounds.width < MIN_KEEP
                && draft.bounds.height < MIN_KEEP
                && !draft.is_pen();
            if !tiny {
                self.history
                    .execute(&mut self.elements, Command::Add(draft));
            }
        }
    }

    pub fn add_text(&mut self, pt: Point, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let el = Element {
            id: self.alloc_id(),
            bounds: Rect::new(
                pt.x,
                pt.y,
                (trimmed.len() as f64 * 12.0).max(40.0),
                28.0,
            ),
            stroke: DEFAULT_STROKE,
            stroke_width: DEFAULT_STROKE_WIDTH,
            shape: Shape::Text {
                text: trimmed.to_string(),
                font_size: 20.0,
            },
        };
        self.history
            .execute(&mut self.elements, Command::Add(el));
    }

    pub fn undo(&mut self) -> bool {
        self.selected_id = None;
        self.history.undo(&mut self.elements)
    }

    pub fn redo(&mut self) -> bool {
        self.selected_id = None;
        self.history.redo(&mut self.elements)
    }

    pub fn clear(&mut self) {
        let snapshot = self.elements.clone();
        self.history
            .execute(&mut self.elements, Command::Clear { snapshot });
        self.selected_id = None;
        self.draft = None;
    }

    pub fn cancel_draft(&mut self) {
        self.draft = None;
        self.dragging = false;
    }

    fn create_draft(&mut self, pt: Point) -> Element {
        let id = self.alloc_id();
        let bounds = Rect::new(pt.x, pt.y, 1.0, 1.0);
        let shape = match self.tool {
            Tool::Arrow => Shape::Arrow {
                start: pt,
                end: pt,
                head: 12.0,
            },
            Tool::Pen => Shape::Pen { points: vec![pt] },
            Tool::Mosaic => Shape::Mosaic {
                block_size: DEFAULT_MOSAIC_BLOCK,
            },
            _ => Shape::Rect,
        };
        let mut el = Element {
            id,
            bounds,
            stroke: DEFAULT_STROKE,
            stroke_width: DEFAULT_STROKE_WIDTH,
            shape,
        };
        if let Shape::Pen { .. } = el.shape {
            recalc_pen_bounds(&mut el);
        }
        el
    }
}

fn recalc_pen_bounds(el: &mut Element) {
    let Shape::Pen { points } = &el.shape else {
        return;
    };
    if points.is_empty() {
        el.bounds = Rect::new(0.0, 0.0, 0.0, 0.0);
        return;
    }
    let min_x = points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    let min_y = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let max_x = points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max);
    let max_y = points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
    el.bounds = Rect::new(
        min_x,
        min_y,
        (max_x - min_x).max(1.0),
        (max_y - min_y).max(1.0),
    );
}

fn update_draft(draft: &mut Element, start: Point, current: Point) {
    match &mut draft.shape {
        Shape::Arrow { end, .. } => {
            *end = current;
            draft.bounds = rect_from_points(start, current);
        }
        Shape::Pen { points } => {
            points.push(current);
            recalc_pen_bounds(draft);
        }
        _ => {
            draft.bounds = rect_from_points(start, current);
        }
    }
}

fn rect_from_points(a: Point, b: Point) -> Rect {
    Rect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        (b.x - a.x).abs().max(1.0),
        (b.y - a.y).abs().max(1.0),
    )
}

/// QQ-style annotation arrow: tapered body (point tail → thick neck) + filled wedge head.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilledArrow {
    pub tail: Point,
    pub neck_left: Point,
    pub neck_right: Point,
    pub left: Point,
    pub right: Point,
    pub tip: Point,
}

impl FilledArrow {
    /// The arrow as one closed polygon, wound tail -> left flank -> tip -> right flank.
    /// Rasterizing this instead of `triangles()` avoids a seam at the neck.
    pub fn outline(self) -> [Point; 6] {
        [
            self.tail,
            self.neck_left,
            self.left,
            self.tip,
            self.right,
            self.neck_right,
        ]
    }

    pub fn triangles(self) -> [(Point, Point, Point); 2] {
        [
            (self.tail, self.neck_left, self.neck_right),
            (self.tip, self.left, self.right),
        ]
    }

    pub fn neck_width(self) -> f64 {
        dist(self.neck_left, self.neck_right)
    }

    pub fn head_width(self) -> f64 {
        dist(self.left, self.right)
    }
}

fn dist(a: Point, b: Point) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    (dx * dx + dy * dy).sqrt()
}

pub fn filled_arrow(start: Point, end: Point, stroke: f64) -> Option<FilledArrow> {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 4.0 {
        return None;
    }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let stroke = stroke.max(1.0);
    let neck_half = (stroke * 1.6 + 1.5).max(4.0);
    let head_half = neck_half * 1.85;
    let mut head_len = (neck_half * 2.4).min(len * 0.28);
    head_len = head_len.max(10.0_f64.min(len * 0.45));
    let neck_at = (len - head_len).max(0.0);
    let at = |along: f64, across: f64| Point {
        x: start.x + ux * along + px * across,
        y: start.y + uy * along + py * across,
    };
    Some(FilledArrow {
        tail: start,
        neck_left: at(neck_at, neck_half),
        neck_right: at(neck_at, -neck_half),
        left: at(neck_at, head_half),
        right: at(neck_at, -head_half),
        tip: end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f64, y: f64) -> Point {
        Point { x, y }
    }

    #[test]
    fn rect_press_move_release_adds_element() {
        let mut s = Session::new();
        s.tool = Tool::Rect;
        assert_eq!(s.pointer_pressed(pt(10.0, 10.0)), PressKind::Capture);
        s.pointer_moved(pt(40.0, 30.0));
        s.pointer_released();
        assert_eq!(s.elements.len(), 1);
        assert!(matches!(s.elements[0].shape, Shape::Rect));
        assert_eq!(s.elements[0].bounds, Rect::new(10.0, 10.0, 30.0, 20.0));
    }

    #[test]
    fn tiny_non_pen_draft_is_discarded() {
        let mut s = Session::new();
        s.tool = Tool::Rect;
        s.pointer_pressed(pt(5.0, 5.0));
        s.pointer_moved(pt(5.5, 5.5));
        s.pointer_released();
        assert!(s.elements.is_empty());
    }

    #[test]
    fn tiny_pen_is_kept() {
        let mut s = Session::new();
        s.tool = Tool::Pen;
        s.pointer_pressed(pt(8.0, 8.0));
        s.pointer_released();
        assert_eq!(s.elements.len(), 1);
        assert!(matches!(s.elements[0].shape, Shape::Pen { .. }));
    }

    #[test]
    fn arrow_and_mosaic_and_text_create_matching_shapes() {
        let mut s = Session::new();
        s.tool = Tool::Arrow;
        s.pointer_pressed(pt(0.0, 0.0));
        s.pointer_moved(pt(20.0, 0.0));
        s.pointer_released();
        assert!(matches!(s.elements[0].shape, Shape::Arrow { .. }));

        s.tool = Tool::Mosaic;
        s.pointer_pressed(pt(0.0, 10.0));
        s.pointer_moved(pt(32.0, 42.0));
        s.pointer_released();
        match &s.elements[1].shape {
            Shape::Mosaic { block_size } => assert_eq!(*block_size, 16),
            other => panic!("{other:?}"),
        }

        s.tool = Tool::Text;
        assert_eq!(s.pointer_pressed(pt(50.0, 50.0)), PressKind::RequestText);
        s.add_text(pt(50.0, 50.0), "hi");
        assert!(matches!(
            s.elements.last().unwrap().shape,
            Shape::Text { .. }
        ));
    }

    #[test]
    fn undo_then_redo_of_an_add() {
        let mut s = Session::new();
        s.tool = Tool::Rect;
        s.pointer_pressed(pt(0.0, 0.0));
        s.pointer_moved(pt(10.0, 10.0));
        s.pointer_released();
        assert_eq!(s.elements.len(), 1);
        assert!(s.undo());
        assert!(s.elements.is_empty());
        assert!(s.redo());
        assert_eq!(s.elements.len(), 1);
    }

    #[test]
    fn new_add_clears_redo_stack() {
        let mut s = Session::new();
        s.tool = Tool::Rect;
        s.pointer_pressed(pt(0.0, 0.0));
        s.pointer_moved(pt(10.0, 10.0));
        s.pointer_released();
        s.undo();
        assert!(s.history.can_redo());
        s.pointer_pressed(pt(20.0, 20.0));
        s.pointer_moved(pt(40.0, 40.0));
        s.pointer_released();
        assert!(!s.history.can_redo());
        assert_eq!(s.elements.len(), 1);
    }

    #[test]
    fn undo_clear_restores_elements() {
        let mut s = Session::new();
        s.tool = Tool::Rect;
        s.pointer_pressed(pt(0.0, 0.0));
        s.pointer_moved(pt(8.0, 8.0));
        s.pointer_released();
        s.clear();
        assert!(s.elements.is_empty());
        s.undo();
        assert_eq!(s.elements.len(), 1);
    }

    #[test]
    fn empty_text_is_ignored() {
        let mut s = Session::new();
        s.add_text(pt(1.0, 1.0), "   ");
        assert!(s.elements.is_empty());
    }

    #[test]
    fn filled_arrow_is_qq_triangle_not_v_stroke() {
        let g = filled_arrow(pt(0.0, 0.0), pt(100.0, 0.0), 3.0).unwrap();
        assert!((g.tip.x - 100.0).abs() < 1e-9);
        assert!((g.tip.y).abs() < 1e-9);
        assert_eq!(g.tail, pt(0.0, 0.0));
        assert!(g.neck_width() >= 8.0, "neck {}", g.neck_width());
        assert!(
            g.head_width() > g.neck_width() + 2.0,
            "head {} neck {}",
            g.head_width(),
            g.neck_width()
        );
        assert!(g.neck_left.x > 40.0 && g.neck_left.x < 100.0);
        assert!(g.left.y > 0.0 && g.right.y < 0.0);
    }

    #[test]
    fn filled_arrow_body_tapers_from_a_point() {
        let g = filled_arrow(pt(0.0, 0.0), pt(80.0, 0.0), 3.0).unwrap();
        let mid_x = g.neck_left.x * 0.4;
        let t = mid_x / g.neck_left.x;
        let half_at_mid = g.neck_width() / 2.0 * t;
        assert!(half_at_mid < g.neck_width() / 2.0 - 1.0);
        assert!(g.tail.x < g.neck_left.x && g.neck_left.x < g.tip.x);
    }

    #[test]
    fn filled_arrow_none_when_too_short() {
        assert!(filled_arrow(pt(1.0, 1.0), pt(1.4, 1.0), 3.0).is_none());
    }
}

//! WinUI `SettingsCard` metrics redrawn in gpui. The numbers are the ones the
//! C# pages already use: 24 page padding, 4 between cards in a group, 68px
//! minimum card height, 12px secondary text.
//!
//! Every widget returns a plain element and leaves the click handler to the
//! caller, so the view keeps `cx.listener` and these stay view-agnostic.

use gpui::{
    div, prelude::*, px, rgb, svg, white, CursorStyle, Div, ElementId, FontWeight, Hsla,
    SharedString, Stateful,
};

use crate::overlay::theme;

/// WinUI's `SettingsCard` floor. Without it a card with a description and one
/// without differ by ~20px and the column reads as ragged.
const CARD_MIN_H: f32 = 68.;
/// Cards stop widening here. Across the full 920px window a label and its
/// control end up at opposite edges, too far apart to scan as one row.
pub const CONTENT_MAX_W: f32 = 720.;

pub fn page_bg() -> Hsla {
    Hsla::from(rgb(0xF3F3F3))
}

pub fn nav_bg() -> Hsla {
    Hsla::from(rgb(0xEBEBEB))
}

pub fn card_bg() -> Hsla {
    Hsla::from(rgb(0xFBFBFB))
}

pub fn input_bg() -> Hsla {
    Hsla::from(rgb(0xFFFFFF))
}

fn hover_bg() -> Hsla {
    Hsla::from(rgb(0xE8E8E8))
}

fn icon(path: &'static str, size: f32, color: Hsla) -> Div {
    div()
        .flex_none()
        .child(svg().path(path).w(px(size)).h(px(size)).text_color(color))
}

/// Title row at the top of every page, with room for a status chip on the
/// right — the C# pages put transient messages beside the header rather than
/// at the bottom of the scroll, where the user has to hunt for them.
pub fn page_header(title: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .max_w(px(CONTENT_MAX_W))
        .h(px(60.))
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .child(
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::primary_fg())
                .child(title.into()),
        )
}

/// A failure in the same accent as a success would read as "saved"; the pill
/// carries the distinction in both its glyph and its colour.
pub fn status_pill(text: impl Into<SharedString>, error: bool) -> Div {
    let (fg, glyph) = if error {
        (theme::danger(), "settings/close.svg")
    } else {
        (theme::accent(), "settings/check.svg")
    };
    div()
        .flex_none()
        .h(px(24.))
        .px(px(10.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .rounded(px(12.))
        .bg(fg.opacity(0.12))
        .child(icon(glyph, 13., fg))
        .child(div().text_size(px(12.)).text_color(fg).child(text.into()))
}

/// Secondary note under a group, with the info glyph the C# pages use.
pub fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(10.))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(6.))
        .child(icon("settings/info.svg", 14., theme::secondary_fg()).mt(px(1.)))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::secondary_fg())
                .child(text.into()),
        )
}

/// `GroupHeaderStyle`.
pub fn group_header(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(20.))
        .mb(px(8.))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::primary_fg())
        .child(text.into())
}

/// Header and description on the left, the caller's control on the right.
pub fn card(header: impl Into<SharedString>, desc: Option<SharedString>) -> Div {
    shell()
        .min_h(px(CARD_MIN_H))
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .child(label(header, desc))
}

/// `card` with the `HeaderIcon` the C# hotkey cards carry.
pub fn icon_card(
    icon_path: &'static str,
    header: impl Into<SharedString>,
    desc: Option<SharedString>,
) -> Div {
    shell()
        .min_h(px(CARD_MIN_H))
        .flex_row()
        .items_center()
        .gap(px(16.))
        .child(icon(icon_path, 20., theme::secondary_fg()))
        .child(label(header, desc).flex_1())
}

/// `ContentAlignment="Vertical"` — label on top, the control stretched below.
/// Only for values too long to sit beside their label: a URL, a key, a model id.
pub fn vcard(header: impl Into<SharedString>, desc: Option<SharedString>) -> Div {
    shell().flex_col().gap(px(8.)).child(label(header, desc))
}

fn shell() -> Div {
    div()
        .flex()
        .w_full()
        .px(px(16.))
        .py(px(12.))
        .rounded(px(4.))
        .bg(card_bg())
        .border_1()
        .border_color(theme::border())
}

fn label(header: impl Into<SharedString>, desc: Option<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(14.))
                .text_color(theme::primary_fg())
                .child(header.into()),
        )
        .children(desc.map(|d| {
            div()
                .text_size(px(12.))
                .text_color(theme::secondary_fg())
                .child(d)
        }))
}

/// A path shown back to the user: monospace, single line, clipped rather than
/// wrapped so a deep path cannot stretch the card.
pub fn path_line(text: impl Into<SharedString>) -> Div {
    div()
        .font_family("Consolas")
        .text_size(px(12.))
        .text_color(theme::secondary_fg())
        .overflow_hidden()
        .text_ellipsis()
        .child(text.into())
}

/// `ToggleSwitch` with `OnContent`/`OffContent` blank, as every C# card sets it.
pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let (track, edge, knob) = if on {
        (theme::accent(), theme::accent(), white())
    } else {
        (
            theme::transparent(),
            theme::secondary_fg(),
            theme::secondary_fg(),
        )
    };
    div()
        .id(id)
        .flex_none()
        .w(px(40.))
        .h(px(20.))
        .rounded(px(10.))
        .bg(track)
        .border_1()
        .border_color(edge)
        .flex()
        .items_center()
        .cursor(CursorStyle::PointingHand)
        .child(
            div()
                .w(px(12.))
                .h(px(12.))
                .rounded(px(6.))
                .bg(knob)
                .ml(if on { px(23.) } else { px(3.) }),
        )
}

/// One cell of the toolbar grid. 118x36 matches the C# `VariableSizedWrapGrid`,
/// which lays them out four to a row — see `TOOLBAR_COLUMNS`.
pub const TOOLBAR_COLUMNS: usize = 4;

pub fn checkbox(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    checked: bool,
) -> Stateful<Div> {
    let (box_bg, box_edge) = if checked {
        (theme::accent(), theme::accent())
    } else {
        (white(), theme::secondary_fg())
    };
    div()
        .id(id)
        .w(px(118.))
        .h(px(36.))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .cursor(CursorStyle::PointingHand)
        .child(
            div()
                .flex_none()
                .w(px(16.))
                .h(px(16.))
                .rounded(px(3.))
                .bg(box_bg)
                .border_1()
                .border_color(box_edge)
                .flex()
                .items_center()
                .justify_center()
                .when(checked, |el| {
                    el.child(icon("settings/check.svg", 11., white()))
                }),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::primary_fg())
                .child(text.into()),
        )
}

pub fn button(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    button_shell(id).px(px(14.)).child(text.into())
}

/// Icon-only button, as the C# "open folder" cards use.
pub fn icon_button(id: impl Into<ElementId>, icon_path: &'static str) -> Stateful<Div> {
    button_shell(id)
        .w(px(32.))
        .child(icon(icon_path, 15., theme::primary_fg()))
}

fn button_shell(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(white())
        .border_1()
        .border_color(theme::border())
        .text_size(px(13.))
        .text_color(theme::primary_fg())
        .cursor(CursorStyle::PointingHand)
        .hover(|s| s.bg(hover_bg()))
}

/// The closed state of a dropdown: current value plus a chevron. `width` lets a
/// short value sit beside its label instead of stretching the card.
pub fn select(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
    open: bool,
    width: Option<f32>,
) -> Stateful<Div> {
    let base = div().id(id);
    let base = match width {
        Some(w) => base.flex_none().w(px(w)),
        None => base.w_full(),
    };
    base.h(px(32.))
        .px(px(8.))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .rounded(px(4.))
        .bg(input_bg())
        .border_1()
        .border_color(if open {
            theme::accent()
        } else {
            theme::border()
        })
        .cursor(CursorStyle::PointingHand)
        .hover(|s| s.bg(hover_bg()))
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::primary_fg())
                .child(value.into()),
        )
        .child(icon("settings/chevron.svg", 12., theme::secondary_fg()))
}

/// One row of an open dropdown.
pub fn select_option(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .h(px(30.))
        .px(px(8.))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .rounded(px(4.))
        .cursor(CursorStyle::PointingHand)
        .hover(|s| s.bg(hover_bg()))
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::primary_fg())
                .child(text.into()),
        )
        .when(selected, |el| {
            el.child(icon("settings/check.svg", 11., theme::accent()))
        })
}

/// Wraps the open option rows so they read as one surface under the field.
pub fn select_list() -> Div {
    div()
        .w_full()
        .mt(px(4.))
        .p(px(4.))
        .flex()
        .flex_col()
        .rounded(px(4.))
        .bg(input_bg())
        .border_1()
        .border_color(theme::border())
}

/// The collapsed right-hand side of an expander card: a summary plus a chevron,
/// matching the C# `SettingsExpander` the Prompt field lives in.
pub fn expander_summary(summary: impl Into<SharedString>, open: bool) -> Div {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::secondary_fg())
                .child(summary.into()),
        )
        .child(icon(
            "settings/chevron.svg",
            14.,
            if open {
                theme::accent()
            } else {
                theme::secondary_fg()
            },
        ))
}

/// The C# `HotkeyInput` box: monospace combo, accent while recording.
pub fn hotkey_field(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    recording: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(200.))
        .h(px(36.))
        .px(px(12.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(if recording {
            theme::active_background()
        } else {
            input_bg()
        })
        .border_1()
        .border_color(if recording {
            theme::accent()
        } else {
            theme::border()
        })
        .cursor(CursorStyle::PointingHand)
        .hover(|s| s.bg(hover_bg()))
        .child(
            div()
                .font_family("Consolas")
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if recording {
                    theme::accent()
                } else {
                    theme::primary_fg()
                })
                .child(text.into()),
        )
}

/// Cancels an in-progress recording. Without it the only ways out are clicking
/// the field again or waiting for the timeout, neither of which is visible.
pub fn cancel_button(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(28.))
        .h(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor(CursorStyle::PointingHand)
        .hover(|s| s.bg(hover_bg()))
        .child(icon("settings/close.svg", 14., theme::secondary_fg()))
}

/// `NavigationViewItem`: 40 tall, accent bar and glyph on the left when
/// selected. The C# pane carries a `FontIcon` per entry.
pub fn nav_item(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    text: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let fg = if selected {
        theme::accent()
    } else {
        theme::secondary_fg()
    };
    div()
        .id(id)
        .h(px(40.))
        .mx(px(4.))
        .my(px(2.))
        .rounded(px(4.))
        .flex()
        .flex_row()
        .items_center()
        .cursor(CursorStyle::PointingHand)
        .when(selected, |el| el.bg(card_bg()))
        .when(!selected, |el| el.hover(|s| s.bg(hover_bg())))
        .child(
            div()
                .flex_none()
                .w(px(3.))
                .h(px(18.))
                .ml(px(4.))
                .rounded(px(2.))
                .bg(if selected {
                    theme::accent()
                } else {
                    theme::transparent()
                }),
        )
        .child(icon(icon_path, 20., fg).ml(px(9.)))
        .child(
            div()
                .ml(px(10.))
                .text_size(px(14.))
                .when(selected, |el| el.font_weight(FontWeight::SEMIBOLD))
                .text_color(theme::primary_fg())
                .child(text.into()),
        )
}

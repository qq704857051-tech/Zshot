//! Isolated GPUI probe. Default builds do not link GPUI so capture/hotkeys
//! cannot fail because of a GPUI compile or runtime issue.

use crate::error::Result;

#[cfg(feature = "gpui-probe")]
pub fn run() -> Result<()> {
    use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, div, prelude::*, px, size};

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(420.0), px(240.0)), cx);
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| ProbeView)
            },
        );
    });
    Ok(())
}

#[cfg(feature = "gpui-probe")]
struct ProbeView;

#[cfg(feature = "gpui-probe")]
impl gpui::Render for ProbeView {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{div, prelude::*, px};
        div()
            .flex()
            .flex_col()
            .p_4()
            .child("Zshot GPUI probe")
            .child("type 中文 here")
            .child(div().h(px(32.0)))
    }
}

#[cfg(not(feature = "gpui-probe"))]
pub fn run() -> Result<()> {
    eprintln!("gpui-probe: not linked (build with --features gpui-probe)");
    eprintln!("capture / hotkeys / tray are unaffected");
    Ok(())
}

pub mod app;
pub mod black;
pub mod capture;
pub mod codec;
pub mod error;
pub mod exclusive;
pub mod geom;
pub mod gpui_probe;
pub mod hdr;
pub mod hotkey;
pub mod overlay;
pub mod perf;
pub mod platform;

pub use app::{once_capture, run};
pub use error::{Error, Result};

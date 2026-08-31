use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub struct ToolbarAssets;

impl AssetSource for ToolbarAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "toolbar/select.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/select.svg"
            ))),
            "toolbar/rect.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/rect.svg"
            ))),
            "toolbar/arrow.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/arrow.svg"
            ))),
            "toolbar/pen.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/pen.svg"
            ))),
            "toolbar/mosaic.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/mosaic.svg"
            ))),
            "toolbar/text.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/text.svg"
            ))),
            "toolbar/undo.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/undo.svg"
            ))),
            "toolbar/redo.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/redo.svg"
            ))),
            "toolbar/clear.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/clear.svg"
            ))),
            "toolbar/ocr.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/ocr.svg"
            ))),
            "toolbar/translate.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/translate.svg"
            ))),
            "toolbar/longCapture.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/longCapture.svg"
            ))),
            "toolbar/pin.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/pin.svg"
            ))),
            "toolbar/save.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/save.svg"
            ))),
            "toolbar/complete.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/complete.svg"
            ))),
            "toolbar/cancel.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/toolbar/cancel.svg"
            ))),
            "settings/screenshot.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/screenshot.svg"
            ))),
            "settings/ocr.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/ocr.svg"
            ))),
            "settings/hotkey.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/hotkey.svg"
            ))),
            "settings/general.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/general.svg"
            ))),
            "settings/copy.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/copy.svg"
            ))),
            "settings/monitor.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/monitor.svg"
            ))),
            "settings/check.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/check.svg"
            ))),
            "settings/info.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/info.svg"
            ))),
            "settings/folder.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/folder.svg"
            ))),
            "settings/chevron.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/chevron.svg"
            ))),
            "settings/close.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../../assets/settings/close.svg"
            ))),
            _ => None,
        })
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path.trim_end_matches('/') == "toolbar" {
            Ok(super::toolbar::PHASE1_ITEMS
                .iter()
                .filter_map(|id| super::toolbar::icon_path(id))
                .map(SharedString::from)
                .collect())
        } else {
            Ok(Vec::new())
        }
    }
}

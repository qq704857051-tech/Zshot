//! DirectWrite + Direct2D rasterization of one text annotation, shared by the
//! export composite so the saved PNG matches the gpui text the overlay preview
//! shows (`OverlayView::text_layer`).

use std::ptr;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, IWICImagingFactory, GUID_WICPixelFormat32bppPBGRA,
    WICBitmapCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use crate::error::Result;

/// Font used by both the overlay preview and the export raster, so what the
/// user sees is what the PNG gets.
pub const FONT_FAMILY: &str = "Microsoft YaHei UI";

fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Premultiplied BGRA bitmap of `text` at `font_size` px (tight `width*4` stride),
/// sized to the laid-out text. `argb` is the fill colour (alpha honoured).
///
/// Caller must have COM initialized already (the overlay UI thread is STA; the
/// export worker thread initializes MTA) -- this never calls CoInitializeEx.
pub fn rasterize(text: &str, font_size: f64, argb: u32) -> Result<(Vec<u8>, u32, u32)> {
    unsafe {
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let family = wide_null(FONT_FAMILY);
        let locale = wide_null("");
        let format = dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size as f32,
            PCWSTR(locale.as_ptr()),
        )?;

        let text_nul = wide_null(text);
        let text_w = &text_nul[..text_nul.len() - 1];
        let max_extent = (1u32 << 15) as f32;
        let layout = dwrite.CreateTextLayout(text_w, &format, max_extent, max_extent)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let width = (metrics.widthIncludingTrailingWhitespace.ceil() as u32).max(1);
        let height = (metrics.height.ceil() as u32).max(1);

        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let bitmap = wic.CreateBitmap(
            width,
            height,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapCacheOnDemand,
        )?;

        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target: ID2D1RenderTarget = d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;

        let a = ((argb >> 24) & 0xFF) as f32 / 255.0;
        let r = ((argb >> 16) & 0xFF) as f32 / 255.0;
        let g = ((argb >> 8) & 0xFF) as f32 / 255.0;
        let b = (argb & 0xFF) as f32 / 255.0;
        let brush = target.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None)?;

        target.BeginDraw();
        target.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));
        target.DrawTextLayout(
            windows_numerics::Vector2 { X: 0.0, Y: 0.0 },
            &layout,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        target.EndDraw(None, None)?;

        let stride = width * 4;
        let mut buf = vec![0u8; (stride * height) as usize];
        bitmap.CopyPixels(ptr::null(), stride, &mut buf)?;
        Ok((buf, width, height))
    }
}

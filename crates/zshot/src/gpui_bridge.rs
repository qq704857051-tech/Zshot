//! GPUI overlay bridge: smoke dual-visual freeze swapchain.

use std::time::Duration;

use gpui::{
    div, hsla, px, rgb, size, App, AppContext, Bounds, Context, IntoElement, ParentElement, Render,
    Styled, Timer, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::error::{Error, Result};

const SMOKE_W: u32 = 800;
const SMOKE_H: u32 = 600;

struct SmokeOverlay {
    /// Keeps the freeze swapchain alive while the window is open.
    _swap: Option<IDXGISwapChain1>,
}

impl Render for SmokeOverlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .text_xl()
            .text_color(rgb(0xffffff))
            .child("Zshot freeze")
    }
}

/// Open a topmost transparent PopUp that shows a red freeze swapchain under GPUI UI.
pub fn open_smoke_overlay(cx: &mut App) -> Result<()> {
    let bounds = Bounds {
        origin: gpui::point(px(120.), px(120.)),
        size: size(px(SMOKE_W as f32), px(SMOKE_H as f32)),
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                topmost: true,
                ..Default::default()
            },
            |_window, cx| cx.new(|_| SmokeOverlay { _swap: None }),
        )
        .map_err(|e| Error::new(e.to_string()))?;

    let swap = match cx.with_gpu_context(|device, context| {
        create_red_composition_swapchain(device, context, SMOKE_W, SMOKE_H)
    }) {
        Some(Ok(swap)) => swap,
        Some(Err(e)) => return Err(e),
        None => return Err(Error::new("GPU context unavailable")),
    };

    handle
        .update(cx, |view, window, _cx| {
            window
                .set_background_swap_chain(Some(&swap))
                .map_err(|e| Error::new(e.to_string()))?;
            view._swap = Some(swap);
            Ok::<(), Error>(())
        })
        .map_err(|e| Error::new(e.to_string()))??;

    cx.spawn(async move |cx| {
        Timer::after(Duration::from_secs(3)).await;
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();

    Ok(())
}

fn create_red_composition_swapchain(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    let swap = unsafe { create_composition_swapchain(device, width, height)? };
    unsafe {
        clear_swapchain_red(device, context, &swap)?;
        context.PSSetShaderResources(0, Some(&[None]));
        context.OMSetRenderTargets(None, None);
        context.ClearState();
    }
    Ok(swap)
}

unsafe fn create_composition_swapchain(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<IDXGISwapChain1> {
    let dxgi_device: IDXGIDevice = device.cast()?;
    let adapter = dxgi_device.GetAdapter()?;
    let factory: IDXGIFactory2 = adapter.GetParent()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: width,
        Height: height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        Flags: 0,
    };
    Ok(factory.CreateSwapChainForComposition(device, &desc, None)?)
}

unsafe fn clear_swapchain_red(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    swap: &IDXGISwapChain1,
) -> Result<()> {
    let buffer: ID3D11Texture2D = swap.GetBuffer(0)?;
    let mut rtv = None;
    device.CreateRenderTargetView(&buffer, None, Some(&mut rtv))?;
    let rtv: ID3D11RenderTargetView = rtv.ok_or_else(|| Error::new("CreateRenderTargetView"))?;
    context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
    context.ClearRenderTargetView(&rtv, &[1.0, 0.0, 0.0, 1.0]);
    context.OMSetRenderTargets(None, None);
    swap.Present(0, DXGI_PRESENT(0)).ok()?;
    Ok(())
}

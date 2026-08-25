mod d3d;
mod dxgi_dup;
mod monitors;
mod wgc;

pub use d3d::{drop_gpu_resources, texture_desc, Gpu, GpuShaders};
pub use monitors::{
    enumerate_monitors, foreground_covers_monitor, monitor_from_foreground, query_monitor_exclusive,
    virtual_desktop, MonitorInfo, VirtualDesktop,
};

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
};

use crate::black::wgc_should_reject_frame;
use crate::error::{Error, Result};
use crate::exclusive::{is_exclusive_fullscreen, should_fallback_to_dxgi, ExclusiveHints};
use crate::hdr::REFERENCE_WHITE_NITS;
use crate::perf::PerfSession;

pub struct CapturedDesktop {
    pub gpu: Gpu,
    /// GPU BGRA8 display texture used as overlay freeze background (no CPU blit).
    pub sdr_display: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub sdr_white_nits: f32,
    pub hdr: bool,
    pub used_dxgi_dup: bool,
    pub exclusive: bool,
}

pub fn capture_for_action(
    region: bool,
    perf: &mut PerfSession,
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    shaders: &GpuShaders,
    overlay_luid: (u32, i32),
) -> Result<CapturedDesktop> {
    let _restore = d3d::RestoreGpu(ctx);
    let _ = overlay_luid;
    let gpu = Gpu::bind(device, shaders);
    let monitors = enumerate_monitors()?;
    if monitors.is_empty() {
        return Err(Error::new("no monitors"));
    }
    let vd = virtual_desktop();
    let fg_idx = monitor_from_foreground(&monitors);
    let focus = fg_idx
        .map(|i| &monitors[i])
        .unwrap_or(&monitors[0]);

    let covers = foreground_covers_monitor(focus);
    let dxgi_exclusive = query_monitor_exclusive(focus);
    let exclusive_hint = ExclusiveHints {
        foreground_covers_monitor: covers,
        dxgi_output_exclusive: dxgi_exclusive,
        wgc_frame_near_black: false,
    };
    let exclusive_guess = is_exclusive_fullscreen(exclusive_hint);

    // Exclusive / fullscreen-copy: capture the focused monitor only.
    // Region on a normal desktop: capture every monitor into the virtual desktop.
    let capture_all = region && !exclusive_guess;

    let targets: Vec<&MonitorInfo> = if capture_all {
        monitors.iter().collect()
    } else {
        vec![focus]
    };

    let any_hdr = targets.iter().any(|m| m.hdr);
    let sdr_white = targets
        .iter()
        .find(|m| m.hdr)
        .map(|m| m.sdr_white_nits)
        .unwrap_or(REFERENCE_WHITE_NITS);

    let mut used_dxgi = false;
    let mut pieces: Vec<(i32, i32, ID3D11Texture2D, bool)> = Vec::new();
    let mut saw_exclusive = exclusive_guess;

    for mon in &targets {
        let want_hdr = any_hdr;
        let mut got: Option<(ID3D11Texture2D, bool, bool)> = None; // tex, hdr, dxgi
        let mut wgc_failed = false;
        let mut wgc_timeout = false;
        let mut wgc_black = false;

        if !exclusive_guess {
            match wgc::capture_monitor(&gpu, ctx, mon, want_hdr) {
                Ok(frame) => {
                    let hdr = frame.hdr;
                    let tex = frame.texture;
                    let near_black = gpu.texture_is_near_black(ctx, &tex);
                    wgc_black = wgc_should_reject_frame(hdr, near_black);
                    if !wgc_black {
                        got = Some((tex, hdr, false));
                    }
                }
                Err(e) => {
                    let msg = e.0.to_lowercase();
                    wgc_timeout = msg.contains("timeout");
                    wgc_failed = true;
                }
            }
        }

        if got.is_none()
            && should_fallback_to_dxgi(exclusive_guess, wgc_failed, wgc_timeout, wgc_black)
        {
            match dxgi_dup::capture_monitor(&gpu, ctx, mon, want_hdr) {
                Ok((tex, hdr)) => {
                    used_dxgi = true;
                    saw_exclusive = is_exclusive_fullscreen(ExclusiveHints {
                        foreground_covers_monitor: covers,
                        dxgi_output_exclusive: dxgi_exclusive,
                        wgc_frame_near_black: wgc_black,
                    });
                    got = Some((tex, hdr, true));
                }
                Err(e) => {
                    if got.is_none() && targets.len() == 1 {
                        return Err(e);
                    }
                }
            }
        }

        if let Some((tex, hdr, _)) = got {
            pieces.push((mon.x, mon.y, tex, hdr));
        }
    }
    perf.mark("T3");

    if pieces.is_empty() {
        return Err(Error::new("no capture frames"));
    }

    let (origin_x, origin_y, width, height) = if capture_all {
        (vd.x, vd.y, vd.w.max(1) as u32, vd.h.max(1) as u32)
    } else {
        (
            targets[0].x,
            targets[0].y,
            targets[0].w.max(1) as u32,
            targets[0].h.max(1) as u32,
        )
    };

    let format = if any_hdr {
        DXGI_FORMAT_R16G16B16A16_FLOAT
    } else {
        DXGI_FORMAT_B8G8R8A8_UNORM
    };
    let composite = d3d::create_texture(
        &gpu.device,
        width,
        height,
        format,
        windows::Win32::Graphics::Direct3D11::D3D11_BIND_SHADER_RESOURCE
            | windows::Win32::Graphics::Direct3D11::D3D11_BIND_RENDER_TARGET,
        windows::Win32::Graphics::Direct3D11::D3D11_USAGE_DEFAULT,
        0,
    )?;

    for (mx, my, tex, _hdr) in &pieces {
        let desc = texture_desc(tex);
        let dst_x = (*mx - origin_x).max(0) as u32;
        let dst_y = (*my - origin_y).max(0) as u32;
        unsafe {
            ctx.CopySubresourceRegion(
                &composite,
                0,
                dst_x,
                dst_y,
                0,
                tex,
                0,
                None,
            );
        }
        let _ = desc;
    }
    perf.mark("T4");

    let sdr_display = gpu.to_sdr_bgra(ctx, &composite, sdr_white)?;
    perf.mark("T5");

    Ok(CapturedDesktop {
        gpu,
        sdr_display,
        width,
        height,
        origin_x,
        origin_y,
        sdr_white_nits: sdr_white,
        hdr: any_hdr,
        used_dxgi_dup: used_dxgi,
        exclusive: saw_exclusive,
    })
}

pub fn crop_sdr_bgra(
    cap: &CapturedDesktop,
    ctx: &ID3D11DeviceContext,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(Vec<u8>, u32, u32, u32)> {
    let _restore = d3d::RestoreGpu(ctx);
    cap.gpu.read_bgra_region(ctx, &cap.sdr_display, x, y, w, h)
}

#[cfg(test)]
mod blob_drop_tests {
    #[test]
    fn d3d_shader_blobs_are_not_manuallydrop() {
        let src = include_str!("d3d.rs");
        assert!(
            !src.contains("ManuallyDrop"),
            "shader blobs must drop after Create*Shader"
        );
        assert!(src.contains("let _blob = blob"));
    }
}

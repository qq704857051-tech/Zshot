mod d3d;
mod dxgi_dup;
mod monitors;
mod wgc;

pub use d3d::{drop_gpu_resources, texture_desc, Gpu, GpuShaders, RestoreGpu};
pub use monitors::{
    dxgi_allowed, enumerate_monitors, foreground_covers_monitor, monitor_from_foreground,
    query_monitor_exclusive, virtual_desktop, MonitorInfo, VirtualDesktop,
};

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
};

use crate::error::{Error, Result};
use crate::exclusive::{is_exclusive_fullscreen, ExclusiveHints};
use crate::hdr::REFERENCE_WHITE_NITS;
use crate::perf::PerfSession;

/// How long a single monitor may take to hand back a frame. This sits on the
/// hotkey-to-overlay path, so it is a user-visible stall, not a background wait.
pub(super) const FRAME_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

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

enum WgcOutcome {
    Ready(wgc::WgcReady),
    /// No usable frame: session never started, wait failed, or worker died.
    Missing,
}

/// WGC sessions started and frames waited off the GPU context borrow.
pub struct PreparedCapture {
    gpu: Gpu,
    monitors: Vec<MonitorInfo>,
    target_indices: Vec<usize>,
    capture_all: bool,
    exclusive_guess: bool,
    covers: bool,
    dxgi_exclusive: bool,
    any_hdr: bool,
    sdr_white: f32,
    overlay_luid: (u32, i32),
    wgc: Vec<WgcOutcome>,
}

/// Start WGC (CreateFreeThreaded) and wait for frames in parallel. Do not call while
/// holding `with_gpu_context` — only the device (via `Gpu`) is needed here.
pub fn prepare_capture(
    region: bool,
    device: &ID3D11Device,
    shaders: &GpuShaders,
    overlay_luid: (u32, i32),
) -> Result<PreparedCapture> {
    let gpu = Gpu::bind(device, shaders);
    let monitors = enumerate_monitors()?;
    if monitors.is_empty() {
        return Err(Error::new("no monitors"));
    }
    let fg_idx = monitor_from_foreground(&monitors);
    let focus_idx = fg_idx.unwrap_or(0);
    let focus = &monitors[focus_idx];

    let covers = foreground_covers_monitor(focus);
    let dxgi_exclusive = query_monitor_exclusive(focus);
    let exclusive_hint = ExclusiveHints {
        foreground_covers_monitor: covers,
        dxgi_output_exclusive: dxgi_exclusive,
        wgc_frame_near_black: false,
    };
    let exclusive_guess = is_exclusive_fullscreen(exclusive_hint);

    let capture_all = region && !exclusive_guess;
    let target_indices: Vec<usize> = if capture_all {
        (0..monitors.len()).collect()
    } else {
        vec![focus_idx]
    };

    let any_hdr = target_indices.iter().any(|&i| monitors[i].hdr);
    let sdr_white = target_indices
        .iter()
        .map(|&i| &monitors[i])
        .find(|m| m.hdr)
        .map(|m| m.sdr_white_nits)
        .unwrap_or(REFERENCE_WHITE_NITS);

    let want_hdr = any_hdr;
    let mut wgc: Vec<WgcOutcome> = Vec::with_capacity(target_indices.len());
    let mut joins = Vec::new();

    for (slot, &idx) in target_indices.iter().enumerate() {
        if exclusive_guess {
            wgc.push(WgcOutcome::Missing);
            continue;
        }
        match wgc::start_monitor(&gpu, &monitors[idx], want_hdr) {
            Ok(pending) => {
                // Placeholder until the wait thread returns.
                wgc.push(WgcOutcome::Missing);
                joins.push(std::thread::spawn(move || (slot, wgc::wait_frame(pending))));
            }
            Err(_) => wgc.push(WgcOutcome::Missing),
        }
    }

    for join in joins {
        // Wait failure or a panicked worker leaves the slot Missing.
        if let Ok((slot, Ok(ready))) = join.join() {
            wgc[slot] = WgcOutcome::Ready(ready);
        }
    }

    Ok(PreparedCapture {
        gpu,
        monitors,
        target_indices,
        capture_all,
        exclusive_guess,
        covers,
        dxgi_exclusive,
        any_hdr,
        sdr_white,
        overlay_luid,
        wgc,
    })
}

/// `copy_to_owned` / DXGI / composite — must run inside `with_gpu_context`.
pub fn capture_for_action(
    prepared: PreparedCapture,
    perf: &mut PerfSession,
    _device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    _shaders: &GpuShaders,
) -> Result<CapturedDesktop> {
    let _restore = d3d::RestoreGpu(ctx);
    let PreparedCapture {
        gpu,
        monitors,
        target_indices,
        capture_all,
        exclusive_guess,
        covers,
        dxgi_exclusive,
        any_hdr,
        sdr_white,
        overlay_luid,
        mut wgc,
    } = prepared;
    let vd = virtual_desktop();
    let want_hdr = any_hdr;

    let mut used_dxgi = false;
    let mut pieces: Vec<(i32, i32, ID3D11Texture2D, bool)> = Vec::new();
    let mut saw_exclusive = exclusive_guess;

    for (slot, &idx) in target_indices.iter().enumerate() {
        let mon = &monitors[idx];
        let mut got: Option<(ID3D11Texture2D, bool)> = None;
        let mut wgc_black = false;

        if let WgcOutcome::Ready(ready) = std::mem::replace(&mut wgc[slot], WgcOutcome::Missing) {
            if let Ok(frame) = ready.into_owned(&gpu, ctx) {
                // A near-black frame means WGC missed an exclusive-fullscreen app.
                wgc_black = gpu.texture_is_near_black(ctx, &frame.texture);
                if !wgc_black {
                    got = Some((frame.texture, frame.hdr));
                }
            }
        }

        // Every way of reaching `got == None` — create failure, wait failure, a
        // near-black frame, or a session skipped under exclusive fullscreen — wants
        // the same DuplicateOutput1 fallback, so there is nothing left to branch on.
        if got.is_none() {
            if dxgi_allowed(mon, overlay_luid) {
                match dxgi_dup::capture_monitor(&gpu, ctx, mon, want_hdr, overlay_luid) {
                    Ok((tex, hdr)) => {
                        used_dxgi = true;
                        saw_exclusive = is_exclusive_fullscreen(ExclusiveHints {
                            foreground_covers_monitor: covers,
                            dxgi_output_exclusive: dxgi_exclusive,
                            wgc_frame_near_black: wgc_black,
                        });
                        got = Some((tex, hdr));
                    }
                    Err(e) => {
                        if target_indices.len() == 1 {
                            return Err(e);
                        }
                    }
                }
            } else if target_indices.len() == 1 {
                return Err(Error::new(
                    "capture unsupported: no WGC frame and DXGI blocked by adapter LUID mismatch",
                ));
            }
        }

        if let Some((tex, hdr)) = got {
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
        let t0 = &monitors[target_indices[0]];
        (t0.x, t0.y, t0.w.max(1) as u32, t0.h.max(1) as u32)
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

    for (mx, my, tex, piece_hdr) in &pieces {
        let dst_x = (*mx - origin_x).max(0) as u32;
        let dst_y = (*my - origin_y).max(0) as u32;
        if any_hdr && !*piece_hdr {
            gpu.white_lift_into(ctx, tex, &composite, dst_x, dst_y, sdr_white)?;
        } else {
            unsafe {
                ctx.CopySubresourceRegion(&composite, 0, dst_x, dst_y, 0, tex, 0, None);
            }
        }
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

use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO,
};

use crate::error::{Error, Result};

use super::d3d::Gpu;
use super::monitors::{dxgi_allowed, MonitorInfo};
use super::FRAME_WAIT;

/// Per-`AcquireNextFrame` wait. The loop retries until `FRAME_WAIT` is spent.
const ACQUIRE_SLICE_MS: u32 = 50;

pub fn capture_monitor(
    gpu: &Gpu,
    ctx: &ID3D11DeviceContext,
    mon: &MonitorInfo,
    want_hdr: bool,
    overlay_luid: (u32, i32),
) -> Result<(ID3D11Texture2D, bool)> {
    if !dxgi_allowed(mon, overlay_luid) {
        return Err(Error::new("DXGI blocked: monitor adapter LUID != overlay"));
    }
    let output = mon
        .output
        .as_ref()
        .ok_or_else(|| Error::new("monitor has no DXGI output"))?;
    let dup: IDXGIOutputDuplication;
    let hdr = want_hdr && mon.hdr;
    unsafe {
        if hdr {
            let output5: IDXGIOutput5 = output.cast()?;
            let fmts = [DXGI_FORMAT_R16G16B16A16_FLOAT];
            dup = output5.DuplicateOutput1(&gpu.device, 0, &fmts)?;
        } else {
            let output1: IDXGIOutput1 = output.cast()?;
            dup = output1.DuplicateOutput(&gpu.device)?;
        }
    }

    let owned = acquire_desktop_frame(&dup, gpu, ctx)?;
    Ok((owned, hdr))
}

/// The first `AcquireNextFrame` after `DuplicateOutput` routinely returns a wake
/// carrying only a pointer update — `LastPresentTime == 0` and a stale surface.
/// Taking that frame is how the exclusive-fullscreen path ends up saving an empty
/// desktop, so keep asking until a real present shows up or the budget runs out.
fn acquire_desktop_frame(
    dup: &IDXGIOutputDuplication,
    gpu: &Gpu,
    ctx: &ID3D11DeviceContext,
) -> Result<ID3D11Texture2D> {
    let deadline = Instant::now() + FRAME_WAIT;
    loop {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let acquired = unsafe { dup.AcquireNextFrame(ACQUIRE_SLICE_MS, &mut info, &mut resource) };

        if acquired.is_ok() {
            let usable = info.LastPresentTime != 0;
            if usable {
                if let Some(res) = resource.as_ref() {
                    let tex: ID3D11Texture2D = res.cast()?;
                    let owned = gpu.copy_to_owned(ctx, &tex)?;
                    drop(tex);
                    drop(resource);
                    unsafe {
                        let _ = dup.ReleaseFrame();
                    }
                    return Ok(owned);
                }
            }
            // Pointer-only wake: hand it back before asking again.
            drop(resource);
            unsafe {
                let _ = dup.ReleaseFrame();
            }
        }

        if Instant::now() >= deadline {
            return Err(Error::new(
                "DXGI DuplicateOutput produced no desktop frame before the deadline",
            ));
        }
    }
}

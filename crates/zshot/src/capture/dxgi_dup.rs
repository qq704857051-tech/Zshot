use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R16G16B16A16_FLOAT};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTPUT_DESC, DXGI_ENUM_MODES_INTERLACED,
};

use crate::error::{Error, Result};

use super::d3d::Gpu;
use super::monitors::MonitorInfo;

pub fn capture_monitor(gpu: &Gpu, mon: &MonitorInfo, want_hdr: bool) -> Result<(ID3D11Texture2D, bool)> {
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
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut resource = None;
    unsafe {
        dup.AcquireNextFrame(10_000, &mut info, &mut resource)
            .map_err(|_| Error::new("DXGI DuplicateOutput AcquireNextFrame timeout/fail"))?;
    }
    let resource = resource.ok_or_else(|| Error::new("DXGI frame resource"))?;
    let tex: ID3D11Texture2D = resource.cast()?;
    let owned = gpu.copy_to_owned(&tex)?;
    unsafe {
        let _ = dup.ReleaseFrame();
    }
    let _ = DXGI_ENUM_MODES_INTERLACED;
    let _ = DXGI_OUTPUT_DESC::default();
    let _ = HMODULE::default();
    let _ = DXGI_FORMAT::default();
    Ok((owned, hdr))
}

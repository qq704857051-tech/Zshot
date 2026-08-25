use std::sync::mpsc;
use std::time::Duration;

use windows::core::{Interface, IInspectable};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use crate::error::{Error, Result};

use super::d3d::Gpu;
use super::monitors::MonitorInfo;

pub struct WgcFrame {
    pub texture: ID3D11Texture2D,
    pub hdr: bool,
}

pub fn capture_monitor(gpu: &Gpu, mon: &MonitorInfo, want_hdr: bool) -> Result<WgcFrame> {
    let item = create_item_for_monitor(mon.handle)?;
    let size = item.Size()?;
    if size.Width <= 0 || size.Height <= 0 {
        return Err(Error::new("WGC item has empty size"));
    }
    let format = if want_hdr {
        DirectXPixelFormat::R16G16B16A16Float
    } else {
        DirectXPixelFormat::B8G8R8A8UIntNormalized
    };
    let device: IDirect3DDevice = gpu.winrt_device()?;
    capture_item(gpu, &item, &device, format, mon.hdr && want_hdr)
}

fn create_item_for_monitor(monitor: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    unsafe { interop.CreateForMonitor(monitor).map_err(Into::into) }
}

fn capture_item(
    gpu: &Gpu,
    item: &GraphicsCaptureItem,
    device: &IDirect3DDevice,
    format: DirectXPixelFormat,
    hdr: bool,
) -> Result<WgcFrame> {
    let size: SizeInt32 = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, format, 1, size)?;
    let session: GraphicsCaptureSession = pool.CreateCaptureSession(item)?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIncludeSecondaryWindows(true);

    let (tx, rx) = mpsc::channel::<Direct3D11CaptureFrame>();
    let token = pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
        move |pool, _| {
            if let Some(pool) = pool.as_ref() {
                if let Ok(frame) = pool.TryGetNextFrame() {
                    let _ = tx.send(frame);
                }
            }
            Ok(())
        },
    ))?;

    session.StartCapture()?;
    let frame = rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| Error::new("WGC timeout after 10 seconds"))?;
    let _ = pool.RemoveFrameArrived(token);
    session.Close()?;

    let surface = frame.Surface()?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
    let src: ID3D11Texture2D = unsafe { access.GetInterface()? };
    let owned = gpu.copy_to_owned(&src)?;
    drop(frame);
    pool.Close()?;
    Ok(WgcFrame {
        texture: owned,
        hdr,
    })
}

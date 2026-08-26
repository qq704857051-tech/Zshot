use std::sync::mpsc;

use windows::core::{Interface, IInspectable};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use crate::error::{Error, Result};

use super::d3d::Gpu;
use super::monitors::MonitorInfo;
use super::FRAME_WAIT;

pub struct WgcFrame {
    pub texture: ID3D11Texture2D,
    pub hdr: bool,
}

/// Free-threaded capture session waiting on its frame channel (no GPU context held).
pub struct WgcPending {
    pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    token: i64,
    rx: mpsc::Receiver<Direct3D11CaptureFrame>,
    hdr: bool,
}

/// Frame arrived; still owns the pool/session until `into_owned` copies off the GPU context.
pub struct WgcReady {
    frame: Option<Direct3D11CaptureFrame>,
    pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    token: i64,
    hdr: bool,
}

pub fn start_monitor(gpu: &Gpu, mon: &MonitorInfo, want_hdr: bool) -> Result<WgcPending> {
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
    start_item(&item, &device, format, mon.hdr && want_hdr)
}

pub fn wait_frame(mut pending: WgcPending) -> Result<WgcReady> {
    let frame = pending
        .rx
        .recv_timeout(FRAME_WAIT)
        .map_err(|_| Error::new("WGC produced no frame before the deadline"))?;
    let pool = pending.pool.take();
    let session = pending.session.take();
    let token = pending.token;
    let hdr = pending.hdr;
    Ok(WgcReady {
        frame: Some(frame),
        pool,
        session,
        token,
        hdr,
    })
}

impl WgcReady {
    pub fn into_owned(mut self, gpu: &Gpu, ctx: &ID3D11DeviceContext) -> Result<WgcFrame> {
        let frame = self
            .frame
            .take()
            .ok_or_else(|| Error::new("WGC frame already taken"))?;
        let pool = self
            .pool
            .take()
            .ok_or_else(|| Error::new("WGC pool already taken"))?;
        let session = self
            .session
            .take()
            .ok_or_else(|| Error::new("WGC session already taken"))?;
        let token = self.token;
        let hdr = self.hdr;

        let _ = pool.RemoveFrameArrived(token);
        session.Close()?;
        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        let src: ID3D11Texture2D = unsafe { access.GetInterface()? };
        let owned = gpu.copy_to_owned(ctx, &src)?;
        drop(frame);
        pool.Close()?;
        Ok(WgcFrame {
            texture: owned,
            hdr,
        })
    }
}

impl Drop for WgcPending {
    fn drop(&mut self) {
        if let (Some(pool), Some(session)) = (self.pool.take(), self.session.take()) {
            let _ = pool.RemoveFrameArrived(self.token);
            let _ = session.Close();
            let _ = pool.Close();
        }
    }
}

impl Drop for WgcReady {
    fn drop(&mut self) {
        let _ = self.frame.take();
        if let (Some(pool), Some(session)) = (self.pool.take(), self.session.take()) {
            let _ = pool.RemoveFrameArrived(self.token);
            let _ = session.Close();
            let _ = pool.Close();
        }
    }
}

fn create_item_for_monitor(monitor: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    unsafe { interop.CreateForMonitor(monitor).map_err(Into::into) }
}

fn start_item(
    item: &GraphicsCaptureItem,
    device: &IDirect3DDevice,
    format: DirectXPixelFormat,
    hdr: bool,
) -> Result<WgcPending> {
    let size: SizeInt32 = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, format, 1, size)?;
    let session: GraphicsCaptureSession = pool.CreateCaptureSession(item)?;
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(false);
    let _ = session.SetIncludeSecondaryWindows(true);

    let (tx, rx) = mpsc::channel::<Direct3D11CaptureFrame>();
    let token = pool.FrameArrived(
        &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
            if let Some(pool) = pool.as_ref() {
                if let Ok(frame) = pool.TryGetNextFrame() {
                    let _ = tx.send(frame);
                }
            }
            Ok(())
        }),
    )?;

    session.StartCapture()?;
    Ok(WgcPending {
        pool: Some(pool),
        session: Some(session),
        token,
        rx,
        hdr,
    })
}

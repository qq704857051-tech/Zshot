//! WIC PNG + CF_DIB clipboard. Used by overlay confirm and --once-capture.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::ptr;
use std::slice;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFOHEADER};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapEncoder, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

use crate::error::{Error, Result};

pub fn encode_png_bgra(pixels: &[u8], width: u32, height: u32, stride: u32) -> Result<Vec<u8>> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream: IStream = CreateStreamOnHGlobal(windows::Win32::Foundation::HGLOBAL::default(), true)?;
        let encoder: IWICBitmapEncoder =
            factory.CreateEncoder(&GUID_ContainerFormatPng, ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        let mut bag = None;
        encoder.CreateNewFrame(&mut frame, &mut bag)?;
        let frame = frame.ok_or_else(|| Error::new("WIC CreateNewFrame"))?;
        frame.Initialize(None)?;
        frame.SetSize(width, height)?;
        let mut pf = GUID_WICPixelFormat32bppBGRA;
        frame.SetPixelFormat(&mut pf)?;
        frame.WritePixels(height, stride, pixels)?;
        frame.Commit()?;
        encoder.Commit()?;

        let hg = GetHGlobalFromStream(&stream)?;
        let ptr = GlobalLock(hg);
        if ptr.is_null() {
            return Err(Error::new("GlobalLock failed for PNG stream"));
        }
        let size = GlobalSize(hg);
        let bytes = slice::from_raw_parts(ptr as *const u8, size).to_vec();
        let _ = GlobalUnlock(hg);
        Ok(trim_png(bytes))
    }
}

fn trim_png(mut bytes: Vec<u8>) -> Vec<u8> {
    const SIG: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.len() < 8 || &bytes[..8] != SIG {
        return bytes;
    }
    let mut i = 8usize;
    while i + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        let ctype = &bytes[i + 4..i + 8];
        let next = i + 12 + len;
        if next > bytes.len() {
            break;
        }
        if ctype == b"IEND" {
            bytes.truncate(next);
            break;
        }
        i = next;
    }
    bytes
}

pub fn write_png_file(path: &Path, pixels: &[u8], width: u32, height: u32, stride: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = encode_png_bgra(pixels, width, height, stride)?;
    let mut f = File::create(path)?;
    f.write_all(&bytes)?;
    Ok(())
}

pub fn set_clipboard_dib(pixels: &[u8], width: u32, height: u32, stride: u32) -> Result<()> {
    let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
    let packed_stride = width * 4;
    let image_size = (packed_stride * height) as usize;
    let total = header_size + image_size;
    unsafe {
        let hg = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total)?;
        let ptr = GlobalLock(hg) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(hg));
            return Err(Error::new("GlobalLock failed for DIB"));
        }
        let header = ptr as *mut BITMAPINFOHEADER;
        (*header).biSize = header_size as u32;
        (*header).biWidth = width as i32;
        (*header).biHeight = -(height as i32);
        (*header).biPlanes = 1;
        (*header).biBitCount = 32;
        (*header).biCompression = BI_RGB.0;
        (*header).biSizeImage = image_size as u32;
        let dest = ptr.add(header_size);
        for y in 0..height as usize {
            let src = y * stride as usize;
            let dst = y * packed_stride as usize;
            std::ptr::copy_nonoverlapping(
                pixels.as_ptr().add(src),
                dest.add(dst),
                packed_stride as usize,
            );
        }
        let _ = GlobalUnlock(hg);

        if let Err(e) = OpenClipboard(Some(HWND::default())) {
            let _ = GlobalFree(Some(hg));
            return Err(e.into());
        }
        let _ = EmptyClipboard();
        let handed_over = SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(hg.0)));
        // Close before reporting: an early return here would leave the whole
        // system's clipboard locked by this process.
        let _ = CloseClipboard();
        match handed_over {
            // On success the clipboard owns the block; freeing it would be a
            // double free.
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(Some(hg));
                Err(e.into())
            }
        }
    }
}

/// Copies OCR / translation output. Mirrors `set_clipboard_dib`, including its
/// close-before-reporting rule.
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    unsafe {
        let hg = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, utf16.len() * 2)?;
        let ptr = GlobalLock(hg) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(Some(hg));
            return Err(Error::new("GlobalLock failed for text"));
        }
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
        let _ = GlobalUnlock(hg);

        if let Err(e) = OpenClipboard(Some(HWND::default())) {
            let _ = GlobalFree(Some(hg));
            return Err(e.into());
        }
        let _ = EmptyClipboard();
        let handed_over = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hg.0)));
        let _ = CloseClipboard();
        match handed_over {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = GlobalFree(Some(hg));
                Err(e.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

    const SIG: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10];

    #[test]
    fn encode_png_has_signature_and_iend() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let pixels: Vec<u8> = vec![
            0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
        ];
        let bytes = encode_png_bgra(&pixels, 2, 2, 8).expect("png");
        assert_eq!(&bytes[..8], SIG, "missing PNG signature");
        assert!(bytes.windows(4).any(|w| w == b"IEND"));
        assert!(bytes.len() > 32);
    }

    /// Diagnosis for 'copy-only pastes nothing': the DIB must come back out
    /// of the clipboard with the same pixels.
    #[test]
    fn clipboard_dib_round_trip() {
        use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
        use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

        let pixels: Vec<u8> = vec![1, 2, 3, 255, 5, 6, 7, 255, 9, 10, 11, 255, 13, 14, 15, 255];
        set_clipboard_dib(&pixels, 2, 2, 8).expect("set_clipboard_dib");

        unsafe {
            OpenClipboard(Some(HWND::default())).expect("OpenClipboard");
            let h = GetClipboardData(CF_DIB.0 as u32).expect("GetClipboardData(CF_DIB)");
            let h = windows::Win32::Foundation::HGLOBAL(h.0 as *mut _);
            let size = GlobalSize(h);
            let ptr = GlobalLock(h) as *const u8;
            assert!(!ptr.is_null());
            let data = std::slice::from_raw_parts(ptr, size);
            let header = &*(data.as_ptr() as *const BITMAPINFOHEADER);
            assert_eq!(header.biWidth, 2);
            assert_eq!(header.biHeight, -2, "top-down DIB");
            let image = &data[std::mem::size_of::<BITMAPINFOHEADER>()..];
            assert_eq!(image, &pixels[..]);
            let _ = GlobalUnlock(h);
            let _ = CloseClipboard();
        }
    }
}

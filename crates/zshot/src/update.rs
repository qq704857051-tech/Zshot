//! Update check against GitHub Releases.
//!
//! Same WinHTTP choice as `translate`: one GET does not justify an HTTP crate.
//! The release source is the compile-time `GITHUB_REPO` constant below.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::error::{Error, Result};

/// `owner/repo` of the GitHub project that publishes releases.
const GITHUB_REPO: &str = "Zyl0812/Zshot";

/// Generous bound for one tiny JSON document on a flaky connection.
const TIMEOUT_SECONDS: u32 = 15;

pub struct Release {
    pub version: String,
    pub url: String,
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// True when `latest` sorts after `current`. Component-wise numeric compare
/// over dotted versions; a `v` prefix and `-beta` suffixes are ignored, which
/// matches how release tags are written.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (slot, piece) in out
            .iter_mut()
            .zip(v.trim_start_matches(['v', 'V']).split(['.', '-', '+']))
        {
            *slot = piece.parse().unwrap_or(0);
        }
        out
    }
    parts(latest) > parts(current)
}

/// Blocking. Call it off the UI thread.
pub fn check_latest() -> Result<Release> {
    let body = get(&format!(
        "https://api.github.com/repos/{GITHUB_REPO}/releases/latest"
    ))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| Error::new("发布信息不是有效的 JSON"))?;
    let tag = json["tag_name"].as_str().unwrap_or_default();
    let url = json["html_url"].as_str().unwrap_or_default();
    if tag.is_empty() || url.is_empty() {
        return Err(Error::new("发布信息缺少 tag_name 或 html_url"));
    }
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        url: url.to_string(),
    })
}

pub fn open_release_page(url: &str) -> Result<()> {
    let verb: Vec<u16> = "open".encode_utf16().chain([0]).collect();
    let target: Vec<u16> = url.encode_utf16().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns a fake HINSTANCE; <= 32 is the error range.
    if result.0 as usize <= 32 {
        return Err(Error::new("无法打开发布页面"));
    }
    Ok(())
}

/// GET the whole body, status-checked. Mirrors `translate::post_json` minus
/// the request body and auth header; GitHub only insists on a User-Agent,
/// which `WinHttpOpen`'s agent string already supplies.
fn get(url: &str) -> Result<String> {
    let wide: Vec<u16> = url.encode_utf16().chain([0]).collect();
    let mut parts = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        // u32::MAX asks WinHttp for pointers into `wide` instead of copies.
        dwSchemeLength: u32::MAX,
        dwHostNameLength: u32::MAX,
        dwUrlPathLength: u32::MAX,
        dwExtraInfoLength: u32::MAX,
        ..Default::default()
    };
    unsafe { WinHttpCrackUrl(&wide, 0, &mut parts) }
        .map_err(|_| Error::new(format!("更新地址无效：{url}")))?;

    let host = unsafe { slice_of(parts.lpszHostName, parts.dwHostNameLength) };
    let mut path = unsafe { slice_of(parts.lpszUrlPath, parts.dwUrlPathLength) };
    path.extend_from_slice(&unsafe { slice_of(parts.lpszExtraInfo, parts.dwExtraInfoLength) });
    let secure = parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;

    unsafe {
        let session = Handle(WinHttpOpen(
            PCWSTR(nul_terminated("Zshot").as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(Error::new("WinHttpOpen failed"));
        }
        let ms = (TIMEOUT_SECONDS * 1000) as i32;
        WinHttpSetTimeouts(session.0, ms, ms, ms, ms)?;

        let connect = Handle(WinHttpConnect(
            session.0,
            PCWSTR(nul_terminated_from(&host).as_ptr()),
            parts.nPort,
            0,
        ));
        if connect.0.is_null() {
            return Err(Error::new("WinHttpConnect failed"));
        }

        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(nul_terminated("GET").as_ptr()),
            PCWSTR(nul_terminated_from(&path).as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            if secure {
                WINHTTP_FLAG_SECURE
            } else {
                Default::default()
            },
        ));
        if request.0.is_null() {
            return Err(Error::new("WinHttpOpenRequest failed"));
        }

        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )?;

        let mut payload = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available)?;
            if available == 0 {
                break;
            }
            let at = payload.len();
            payload.resize(at + available as usize, 0u8);
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                payload.as_mut_ptr().add(at) as *mut c_void,
                available,
                &mut read,
            )?;
            payload.truncate(at + read as usize);
            if read == 0 {
                break;
            }
        }

        if !(200..300).contains(&status) {
            return Err(Error::new(format!("检查更新失败（HTTP {status}）")));
        }
        String::from_utf8(payload).map_err(|_| Error::new("发布信息不是有效的 UTF-8"))
    }
}

/// Closes on drop, so every early return above still releases the handle.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// `lpszX`/`dwXLength` pairs point into the caller's cracked URL; copy out.
unsafe fn slice_of(p: windows::core::PWSTR, len: u32) -> Vec<u16> {
    if p.is_null() || len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(p.0, len as usize) }.to_vec()
}

fn nul_terminated(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn nul_terminated_from(s: &[u16]) -> Vec<u16> {
    s.iter().copied().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_ignores_prefix_and_padding() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.2", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
    }
}

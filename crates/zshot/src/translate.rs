//! OpenAI-compatible translation through the user's own API.
//!
//! WinHTTP rather than an HTTP crate: this binary's size and cold start are the
//! point of the Rust port, and a full async stack buys nothing for one POST.

use std::ffi::c_void;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Security::Credentials::{PasswordCredential, PasswordVault};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};

use crate::config;
use crate::error::{Error, Result};

/// Same resource/account as the C# build, so one key serves both.
const VAULT_RESOURCE: &str = "Zshot.Translation";
const VAULT_ACCOUNT: &str = "apiKey";

const DEFAULT_SYSTEM_PROMPT: &str = "You are a translator. Output only the translation. Keep paragraph structure. Do not explain. Do not translate URLs, file paths, or code. Keep numbers and units.";

/// Blocking. Call it off the UI thread.
pub fn translate(text: &str) -> Result<String> {
    let cfg = config::get().translation;
    if cfg.base_url.trim().is_empty() {
        let path = config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        return Err(Error::new(format!(
            "翻译未配置：在 {path} 里填 translation.baseUrl"
        )));
    }
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system_prompt(&cfg.prompt, &cfg.target_language) },
            { "role": "user", "content": text },
        ],
        "temperature": 0.2,
    })
    .to_string();

    let response = post_json(
        &endpoint(&cfg.base_url),
        &api_key()?,
        &body,
        cfg.timeout_seconds.max(5),
    )?;
    message_content(&response)
        .ok_or_else(|| Error::new("Translation API returned an empty result."))
}

/// The vault entry is optional: local gateways commonly take no key.
pub fn api_key() -> Result<String> {
    let vault = PasswordVault::new()?;
    let Ok(cred) = vault.Retrieve(
        &HSTRING::from(VAULT_RESOURCE),
        &HSTRING::from(VAULT_ACCOUNT),
    ) else {
        return Ok(String::new());
    };
    cred.RetrievePassword()?;
    Ok(cred.Password()?.to_string())
}

/// Replaces the stored key. An empty value removes the entry rather than
/// storing a blank one, so `api_key` keeps reporting "no key" the same way it
/// does before the user ever opens the settings window.
pub fn set_api_key(key: &str) -> Result<()> {
    let vault = PasswordVault::new()?;
    let resource = HSTRING::from(VAULT_RESOURCE);
    let account = HSTRING::from(VAULT_ACCOUNT);
    if let Ok(existing) = vault.Retrieve(&resource, &account) {
        vault.Remove(&existing)?;
    }
    let key = key.trim();
    if key.is_empty() {
        return Ok(());
    }
    let cred =
        PasswordCredential::CreatePasswordCredential(&resource, &account, &HSTRING::from(key))?;
    vault.Add(&cred)?;
    Ok(())
}

/// Accept a bare host, a `/v1` root, or the full path — users paste all three.
fn endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        return base.to_string();
    }
    if base.ends_with("/v1") {
        return format!("{base}/chat/completions");
    }
    format!("{base}/v1/chat/completions")
}

fn system_prompt(custom: &str, target_language: &str) -> String {
    let base = if custom.trim().is_empty() {
        DEFAULT_SYSTEM_PROMPT
    } else {
        custom.trim()
    };
    format!("{base}\nTarget language: {target_language}")
}

/// `choices[0].message.content`, with the streaming and Responses-API shapes as
/// fallbacks. Gateways return all three.
fn message_content(json: &str) -> Option<String> {
    // An HTML error page from a proxy is not worth handing to the JSON parser.
    if json.trim_start().starts_with('<') {
        return None;
    }
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let first = root.get("choices").and_then(|c| c.get(0));
    let text = first
        .and_then(|c| c.pointer("/message/content"))
        .or_else(|| first.and_then(|c| c.pointer("/delta/content")))
        .or_else(|| root.get("output_text"))
        .and_then(|v| v.as_str())?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Closes on every path, including the error returns below.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

fn post_json(url: &str, api_key: &str, body: &str, timeout_seconds: u32) -> Result<String> {
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
        .map_err(|_| Error::new(format!("Translation Base URL is not a valid URL: {url}")))?;

    let host = unsafe { slice_of(parts.lpszHostName, parts.dwHostNameLength) };
    let mut path = unsafe { slice_of(parts.lpszUrlPath, parts.dwUrlPathLength) };
    path.extend_from_slice(&unsafe { slice_of(parts.lpszExtraInfo, parts.dwExtraInfoLength) });
    let secure = parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;

    let mut headers = String::from("Content-Type: application/json\r\n");
    if !api_key.trim().is_empty() {
        headers.push_str(&format!("Authorization: Bearer {}\r\n", api_key.trim()));
    }

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
        // One budget for the whole exchange, matching the C# provider, which
        // times the request rather than each socket phase.
        let ms = (timeout_seconds * 1000) as i32;
        WinHttpSetTimeouts(session.0, ms, ms, ms, ms)?;

        let host_w = nul_terminated_from(&host);
        let connect = Handle(WinHttpConnect(
            session.0,
            PCWSTR(host_w.as_ptr()),
            parts.nPort,
            0,
        ));
        if connect.0.is_null() {
            return Err(Error::new("WinHttpConnect failed"));
        }

        let verb = nul_terminated("POST");
        let path_w = nul_terminated_from(&path);
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
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

        let header_w: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(
            request.0,
            Some(&header_w),
            Some(body.as_ptr() as *const c_void),
            body.len() as u32,
            body.len() as u32,
            0,
        )?;
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
            // The body often names the real cause (bad key, unknown model), but
            // it is the provider's text — keep it short and never log the key.
            return Err(Error::new(format!("Translation API failed ({status}).")));
        }
        String::from_utf8(payload)
            .map_err(|_| Error::new("Translation API returned invalid UTF-8."))
    }
}

unsafe fn slice_of(p: PWSTR, len: u32) -> Vec<u16> {
    unsafe {
        if p.is_null() || len == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(p.0, len as usize).to_vec()
    }
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
    fn endpoint_accepts_the_three_shapes_users_paste() {
        let full = "https://api.openai.com/v1/chat/completions";
        assert_eq!(endpoint("https://api.openai.com"), full);
        assert_eq!(endpoint("https://api.openai.com/"), full);
        assert_eq!(endpoint("https://api.openai.com/v1"), full);
        assert_eq!(endpoint("https://api.openai.com/v1/"), full);
        assert_eq!(
            endpoint("  https://api.openai.com/v1/chat/completions  "),
            full
        );
    }

    #[test]
    fn a_custom_prompt_replaces_the_default_but_keeps_the_target_line() {
        assert_eq!(
            system_prompt("", "zh-CN"),
            format!("{DEFAULT_SYSTEM_PROMPT}\nTarget language: zh-CN")
        );
        assert_eq!(
            system_prompt("  只译不解释  ", "en"),
            "只译不解释\nTarget language: en"
        );
    }

    #[test]
    fn reads_the_standard_chat_completion_shape() {
        let json = r#"{"choices":[{"message":{"role":"assistant","content":"  你好世界  "}}]}"#;
        assert_eq!(message_content(json).as_deref(), Some("你好世界"));
    }

    #[test]
    fn falls_back_to_delta_and_output_text() {
        assert_eq!(
            message_content(r#"{"choices":[{"delta":{"content":"partial"}}]}"#).as_deref(),
            Some("partial")
        );
        assert_eq!(
            message_content(r#"{"output_text":"responses api"}"#).as_deref(),
            Some("responses api")
        );
    }

    #[test]
    fn malformed_bodies_yield_none_rather_than_panicking() {
        for json in [
            "",
            "<html><body>502 Bad Gateway</body></html>",
            "{",
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{}}]}"#,
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
            r#"{"error":{"message":"bad key"}}"#,
        ] {
            assert!(message_content(json).is_none(), "{json:?}");
        }
    }
}

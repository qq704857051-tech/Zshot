//! `settings.json` in `%LOCALAPPDATA%\Zshot`. Read once on first use.
//!
//! The API key is deliberately absent: it stays in the Windows credential
//! vault, shared with the C# build. See `translate::api_key`.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub ocr: Ocr,
    pub translation: Translation,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Ocr {
    /// Overrides the default `<exe dir>\models\v6`.
    pub models_dir: Option<PathBuf>,
}

/// Defaults match the C# `AppConfig` so both builds behave the same when the
/// file is missing.
#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Translation {
    pub base_url: String,
    pub model: String,
    pub target_language: String,
    pub prompt: String,
    pub timeout_seconds: u32,
}

impl Default for Translation {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: "gpt-4o-mini".into(),
            target_language: "zh-CN".into(),
            prompt: String::new(),
            timeout_seconds: 30,
        }
    }
}

pub fn path() -> Result<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").map_err(|_| Error::new("LOCALAPPDATA is not set"))?;
    Ok(PathBuf::from(base).join("Zshot").join("settings.json"))
}

pub fn get() -> &'static Settings {
    static CACHE: OnceLock<Settings> = OnceLock::new();
    CACHE.get_or_init(|| {
        let text = path().ok().and_then(|p| std::fs::read_to_string(p).ok());
        parse(text.as_deref().unwrap_or(""))
    })
}

/// A missing or malformed file falls back to defaults — a screenshot tool must
/// still capture when its settings are broken.
fn parse(text: &str) -> Settings {
    if text.trim().is_empty() {
        return Settings::default();
    }
    serde_json::from_str(text).unwrap_or_else(|e| {
        // Hand-edited, with no settings UI behind it. Falling back in silence
        // reads as "translation is broken" with no clue why.
        eprintln!("settings.json ignored: {e}");
        Settings::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_broken_file_falls_back_to_defaults() {
        for text in ["", "not json", "{", "[]"] {
            let s = parse(text);
            assert_eq!(s.translation.model, "gpt-4o-mini", "{text:?}");
            assert_eq!(s.translation.target_language, "zh-CN", "{text:?}");
            assert_eq!(s.translation.timeout_seconds, 30, "{text:?}");
            assert!(s.translation.base_url.is_empty(), "{text:?}");
            assert!(s.ocr.models_dir.is_none(), "{text:?}");
        }
    }

    #[test]
    fn absent_fields_keep_their_defaults() {
        let s = parse(r#"{"translation": {"baseUrl": "https://api.example.com/v1"}}"#);
        assert_eq!(s.translation.base_url, "https://api.example.com/v1");
        assert_eq!(s.translation.model, "gpt-4o-mini");
        assert_eq!(s.translation.timeout_seconds, 30);
    }

    #[test]
    fn keys_are_camel_case() {
        let s = parse(
            r#"{
              "ocr": {"modelsDir": "D:/models/v6"},
              "translation": {
                "baseUrl": "https://x/v1",
                "model": "qwen-plus",
                "targetLanguage": "en",
                "prompt": "be terse",
                "timeoutSeconds": 5
              }
            }"#,
        );
        assert_eq!(
            s.ocr.models_dir.as_deref(),
            Some(std::path::Path::new("D:/models/v6"))
        );
        assert_eq!(s.translation.base_url, "https://x/v1");
        assert_eq!(s.translation.model, "qwen-plus");
        assert_eq!(s.translation.target_language, "en");
        assert_eq!(s.translation.prompt, "be terse");
        assert_eq!(s.translation.timeout_seconds, 5);
    }
}

//! Local PP-OCRv6 recognition. Screenshots never leave the machine.

pub mod reading_order;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use image::RgbImage;
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{OAROCRBuilder, OAROCR};

use crate::error::{Error, Result};
use reading_order::Block;

const DET_MODEL: &str = "PP-OCRv6_det_small.onnx";
const REC_MODEL: &str = "PP-OCRv6_rec_small.onnx";
const DICT: &str = "ppocrv6_dict.txt";

/// Loading the models costs ~160 ms, so the session is worth keeping between
/// clicks — but a tray-resident app must not sit on an ORT arena forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

struct Engine {
    ocr: OAROCR,
    last_used: Instant,
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// Blocking: ~130 ms warm, ~290 ms cold. Call it off the UI thread.
pub fn recognize(bgra: &[u8], w: u32, h: u32, stride: u32) -> Result<String> {
    let image = to_rgb(bgra, w, h, stride)?;

    let mut guard = lock();
    if guard.is_none() {
        *guard = Some(Engine {
            ocr: build()?,
            last_used: Instant::now(),
        });
    }
    let engine = guard.as_mut().expect("just populated");
    let result = engine.ocr.predict(vec![image]);
    engine.last_used = Instant::now();
    drop(guard);
    schedule_idle_drop();

    let pages = result.map_err(|e| Error::new(format!("OCR failed: {e}")))?;
    let blocks: Vec<Block> = pages
        .into_iter()
        .flat_map(|page| page.text_regions)
        .filter_map(|region| {
            Some(Block {
                text: region.text?.to_string(),
                poly: region
                    .bounding_box
                    .points
                    .iter()
                    .map(|p| (p.x as f64, p.y as f64))
                    .collect(),
            })
        })
        .collect();
    Ok(reading_order::to_plain_text(&blocks))
}

/// What the settings page reports as the active engine. There is no fallback
/// recognizer in this build, so a missing model means OCR is simply off.
pub fn models_available() -> bool {
    let Ok(dir) = models_dir() else {
        return false;
    };
    [DET_MODEL, REC_MODEL, DICT]
        .iter()
        .all(|f| dir.join(f).is_file())
}

/// A panic while holding the lock must not disable OCR for the rest of the
/// session — the session behind it is still usable.
fn lock() -> std::sync::MutexGuard<'static, Option<Engine>> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner())
}

fn build() -> Result<OAROCR> {
    let dir = models_dir()?;
    for file in [DET_MODEL, REC_MODEL, DICT] {
        if !dir.join(file).is_file() {
            return Err(Error::new(format!(
                "OCR model missing: {}",
                dir.join(file).display()
            )));
        }
    }
    OAROCRBuilder::new(
        dir.join(DET_MODEL).to_string_lossy().into_owned(),
        dir.join(REC_MODEL).to_string_lossy().into_owned(),
        dir.join(DICT).to_string_lossy().into_owned(),
    )
    // PaddleOCR's own PP-OCRv6 defaults; they hold up on screen text.
    .text_detection_config(TextDetectionConfig {
        score_threshold: 0.2,
        box_threshold: 0.45,
        unclip_ratio: 1.4,
        max_candidates: 3000,
        ..Default::default()
    })
    .build()
    .map_err(|e| Error::new(format!("OCR init failed: {e}")))
}

/// `settings.json` wins, then `ZSHOT_MODELS`, then the install layout the C#
/// build already uses, then the in-repo copy a `cargo run` build sits beside.
fn models_dir() -> Result<PathBuf> {
    if let Some(dir) = crate::config::get().ocr.models_dir {
        return Ok(dir);
    }
    if let Some(dir) = std::env::var_os("ZSHOT_MODELS") {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe()?;
    let base = exe
        .parent()
        .ok_or_else(|| Error::new("executable has no parent directory"))?;
    let shipped = base.join("models").join("v6");
    if shipped.is_dir() {
        return Ok(shipped);
    }
    // ponytail: a `cargo run` build sits in `target\`, where the packaged
    // `models\v6` is not. The repo copy is the same three files, so fall back
    // to it rather than making every developer set ZSHOT_MODELS.
    Ok(PathBuf::from(REPO_MODELS))
}

/// The in-repo copy, resolved at compile time. Absent on a user machine, which
/// is why the directory next to the exe is checked first.
const REPO_MODELS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../models/v6");

fn schedule_idle_drop() {
    // ponytail: one detached timer per recognition. Nobody clicks OCR fast
    // enough for the thread churn to show up; a shared timer if that changes.
    std::thread::spawn(|| {
        std::thread::sleep(IDLE_TIMEOUT);
        let mut guard = lock();
        if guard
            .as_ref()
            .is_some_and(|e| e.last_used.elapsed() >= IDLE_TIMEOUT)
        {
            *guard = None;
        }
    });
}

fn to_rgb(bgra: &[u8], w: u32, h: u32, stride: u32) -> Result<RgbImage> {
    if w == 0 || h == 0 {
        return Err(Error::new("OCR input bitmap is invalid"));
    }
    let need = (h - 1) as usize * stride as usize + w as usize * 4;
    if bgra.len() < need {
        return Err(Error::new("OCR input bitmap is invalid"));
    }
    let mut buf = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h as usize {
        let row = &bgra[y * stride as usize..];
        for x in 0..w as usize {
            let p = &row[x * 4..];
            buf.extend_from_slice(&[p[2], p[1], p[0]]);
        }
    }
    RgbImage::from_raw(w, h, buf).ok_or_else(|| Error::new("OCR input bitmap is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_rgb_drops_alpha_and_honours_stride() {
        // 2x2 BGRA with 4 bytes of row padding.
        let stride = 2 * 4 + 4;
        let mut bgra = vec![0u8; stride * 2];
        for (i, px) in [[10, 20, 30], [40, 50, 60], [70, 80, 90], [100, 110, 120]]
            .iter()
            .enumerate()
        {
            let at = (i / 2) * stride + (i % 2) * 4;
            bgra[at..at + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let img = to_rgb(&bgra, 2, 2, stride as u32).expect("valid bitmap");
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10]);
        assert_eq!(img.get_pixel(1, 0).0, [60, 50, 40]);
        assert_eq!(img.get_pixel(0, 1).0, [90, 80, 70]);
        assert_eq!(img.get_pixel(1, 1).0, [120, 110, 100]);
    }

    #[test]
    fn to_rgb_rejects_short_buffers() {
        assert!(to_rgb(&[0; 4], 2, 2, 8).is_err());
        assert!(to_rgb(&[], 0, 0, 0).is_err());
    }
}

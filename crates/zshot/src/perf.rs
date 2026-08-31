use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use crate::error::Result;

pub struct PerfSession {
    pub t0: Instant,
    marks: Vec<(&'static str, u128)>,
}

impl PerfSession {
    pub fn start() -> Self {
        Self {
            t0: Instant::now(),
            marks: Vec::new(),
        }
    }

    pub fn mark(&mut self, name: &'static str) {
        self.marks.push((name, self.t0.elapsed().as_millis()));
    }

    pub fn mark_ms(&self, name: &str) -> Option<u128> {
        self.marks
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, ms)| *ms)
    }

    pub fn has_mark(&self, name: &str) -> bool {
        self.mark_ms(name).is_some()
    }

    /// Elapsed ms from mark `from` to mark `to` (absolute timestamps on the same session).
    pub fn delta_ms(&self, from: &str, to: &str) -> Option<u128> {
        let a = self.mark_ms(from)?;
        let b = self.mark_ms(to)?;
        Some(b.saturating_sub(a))
    }

    pub fn write_log(&self) -> Result<PathBuf> {
        let dir = log_dir()?;
        create_dir_all(&dir)?;
        let path = dir.join("perf.log");
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        let ts = now_stamp();
        write!(f, "{ts}")?;
        for (name, ms) in &self.marks {
            write!(f, " {name}={ms}ms")?;
        }
        writeln!(f)?;
        Ok(path)
    }
}

/// Nearest-rank percentile on a pre-sorted ascending slice.
/// `p` is in 0..=100. Empty input → 0.
pub fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let p = p.clamp(0.0, 100.0);
    if p <= 0.0 {
        return sorted[0];
    }
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

pub fn write_perf_summary(mode: &str, samples_ms: &mut [u128]) -> Result<PathBuf> {
    samples_ms.sort_unstable();
    let n = samples_ms.len();
    let p50 = percentile(samples_ms, 50.0);
    let p95 = percentile(samples_ms, 95.0);
    let p99 = percentile(samples_ms, 99.0);
    let dir = log_dir()?;
    create_dir_all(&dir)?;
    let path = dir.join("perf-summary.json");
    let body = format!(
        "{{\n  \"mode\": \"{mode}\",\n  \"n\": {n},\n  \"t2_t7_ms\": {{ \"p50\": {p50}, \"p95\": {p95}, \"p99\": {p99} }}\n}}\n"
    );
    std::fs::write(&path, body)?;
    Ok(path)
}

pub fn log_dir() -> Result<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").map_err(|_| missing_localappdata())?;
    Ok(PathBuf::from(base).join("Zshot").join("logs"))
}

/// Append one line to `logs/app.log`. Release builds have no console, so this
/// is where operational failures (capture, export, clipboard) end up. Never
/// fails loudly: a logging error must not break a screenshot.
pub fn log_app(msg: &str) {
    let Ok(dir) = log_dir() else {
        return;
    };
    if create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    else {
        return;
    };
    let _ = writeln!(f, "{} {msg}", now_stamp());
}

fn missing_localappdata() -> crate::error::Error {
    crate::error::Error::new("LOCALAPPDATA is not set")
}

fn now_stamp() -> String {
    // Local time via Windows would be nicer; this is enough to correlate runs.
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("t={}", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p95_of_20_is_near_tail() {
        let v: Vec<u128> = (1..=20).collect();
        assert_eq!(percentile(&v, 50.0), 10);
        assert!(percentile(&v, 95.0) >= 19);
    }

    #[test]
    fn percentile_empty_is_zero() {
        assert_eq!(percentile(&[], 50.0), 0);
    }
}

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

pub fn log_dir() -> Result<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").map_err(|_| missing_localappdata())?;
    Ok(PathBuf::from(base).join("Zshot").join("logs"))
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

//! OCR worker placeholder. Phase 1 only starts and idle-exits.
//! The real PP-OCRv6 Small + ORT session lands in a later phase.

fn main() {
    // Idle-exit: keep the binary launchable without loading models.
    std::thread::sleep(std::time::Duration::from_millis(30));
}

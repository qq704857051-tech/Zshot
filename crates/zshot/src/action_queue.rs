use crate::hotkey::HotkeyAction;

pub const WM_APP_READY_SENTINEL: u32 = 0x8000 + 32; // WM_APP + 32

pub struct ActionQueue {
    tx: std::sync::mpsc::Sender<HotkeyAction>,
}

impl ActionQueue {
    pub fn new() -> (Self, std::sync::mpsc::Receiver<HotkeyAction>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { tx }, rx)
    }

    pub fn send(&self, action: HotkeyAction) {
        let _ = self.tx.send(action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::HotkeyAction;

    #[test]
    fn send_is_received_in_order() {
        let (q, rx) = ActionQueue::new();
        q.send(HotkeyAction::Region);
        q.send(HotkeyAction::CopyOnly);
        assert_eq!(rx.recv().unwrap(), HotkeyAction::Region);
        assert_eq!(rx.recv().unwrap(), HotkeyAction::CopyOnly);
    }

    #[test]
    fn sentinel_is_wm_app_plus_32() {
        assert_eq!(WM_APP_READY_SENTINEL, 0x8020);
    }
}

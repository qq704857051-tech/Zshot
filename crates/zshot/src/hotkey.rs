//! Hotkey matching used by the low-level hook. Tests call these functions
//! without a message loop.

pub const VK_SNAPSHOT: u16 = 0x2C;
pub const VK_A: u16 = 0x41;
pub const VK_W: u16 = 0x57;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    Region,
    CopyOnly,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

impl Hotkey {
    pub const fn new(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u16) -> Self {
        Self {
            ctrl,
            alt,
            shift,
            win,
            vk,
        }
    }

    pub fn matches(&self, ev: KeyEvent) -> bool {
        self.vk == ev.vk
            && self.ctrl == ev.ctrl
            && self.alt == ev.alt
            && self.shift == ev.shift
            && self.win == ev.win
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyBindings {
    pub region: Hotkey,
    pub copy_only: Hotkey,
    pub fullscreen: Hotkey,
}

impl HotkeyBindings {
    /// PoC defaults: PrtSc / Ctrl+Alt+A / Ctrl+Alt+W.
    pub fn poc_defaults() -> Self {
        Self {
            region: Hotkey::new(false, false, false, false, VK_SNAPSHOT),
            copy_only: Hotkey::new(true, true, false, false, VK_A),
            fullscreen: Hotkey::new(true, true, false, false, VK_W),
        }
    }

    pub fn binding_for(&self, action: HotkeyAction) -> Hotkey {
        match action {
            HotkeyAction::Region => self.region,
            HotkeyAction::CopyOnly => self.copy_only,
            HotkeyAction::Fullscreen => self.fullscreen,
        }
    }

    pub fn action_for_combo(&self, ev: KeyEvent) -> Option<HotkeyAction> {
        if self.region.matches(ev) {
            return Some(HotkeyAction::Region);
        }
        if self.copy_only.matches(ev) {
            return Some(HotkeyAction::CopyOnly);
        }
        if self.fullscreen.matches(ev) {
            return Some(HotkeyAction::Fullscreen);
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key_up: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyDecision {
    pub action: Option<HotkeyAction>,
    pub swallow: bool,
}

/// Decide whether this keyboard event is a bound screenshot combo.
///
/// `VK_SNAPSHOT` often arrives as KeyUp-only; we fire the action on KeyUp for
/// PrintScreen and on KeyDown for every other key. Both edges are swallowed
/// when the modifiers match a binding. Unbound variants (e.g. Win+PrtSc when
/// region is plain PrtSc) are not swallowed.
pub fn decide(bindings: &HotkeyBindings, ev: KeyEvent) -> HotkeyDecision {
    let bound = bindings.action_for_combo(ev);
    let swallow = bound.is_some();
    let action = bound.filter(|_| {
        if ev.vk == VK_SNAPSHOT {
            ev.key_up
        } else {
            !ev.key_up
        }
    });
    HotkeyDecision { action, swallow }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(vk: u16, ctrl: bool, alt: bool, shift: bool, win: bool, key_up: bool) -> KeyEvent {
        KeyEvent {
            vk,
            ctrl,
            alt,
            shift,
            win,
            key_up,
        }
    }

    #[test]
    fn prtsc_keyup_only_fires_region() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_SNAPSHOT, false, false, false, false, true));
        assert_eq!(d.action, Some(HotkeyAction::Region));
        assert!(d.swallow);
    }

    #[test]
    fn prtsc_keydown_swallows_but_does_not_double_fire() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_SNAPSHOT, false, false, false, false, false));
        assert_eq!(d.action, None);
        assert!(d.swallow);
    }

    #[test]
    fn win_prtsc_is_not_swallowed_when_unbound() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_SNAPSHOT, false, false, false, true, true));
        assert_eq!(d.action, None);
        assert!(!d.swallow);
    }

    #[test]
    fn ctrl_alt_a_keydown_fires_copy_only() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_A, true, true, false, false, false));
        assert_eq!(d.action, Some(HotkeyAction::CopyOnly));
        assert!(d.swallow);
    }

    #[test]
    fn ctrl_alt_a_with_shift_does_not_match() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_A, true, true, true, false, false));
        assert_eq!(d.action, None);
        assert!(!d.swallow);
    }

    #[test]
    fn ctrl_alt_w_fires_fullscreen() {
        let b = HotkeyBindings::poc_defaults();
        let d = decide(&b, ev(VK_W, true, true, false, false, false));
        assert_eq!(d.action, Some(HotkeyAction::Fullscreen));
        assert!(d.swallow);
    }
}

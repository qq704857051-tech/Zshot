//! Exclusive-fullscreen heuristic. Uncertain cases try WGC first; this only
//! decides when DXGI duplication should be preferred / forced after a black WGC frame.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExclusiveHints {
    pub foreground_covers_monitor: bool,
    pub dxgi_output_exclusive: bool,
    pub wgc_frame_near_black: bool,
}

/// DXGI `AttachedToDesktop == false` means the output is not in the DWM desktop
/// (typical exclusive fullscreen). The shell QUNS D3D-fullscreen flag is the
/// other DXGI/D3D exclusive signal.
pub fn dxgi_output_is_exclusive(
    attached_to_desktop: bool,
    d3d_fullscreen_notification: bool,
) -> bool {
    !attached_to_desktop || d3d_fullscreen_notification
}

/// True when DXGI reports exclusive ownership, or a covering foreground window
/// plus a near-black WGC frame (typical exclusive-fullscreen miss).
pub fn is_exclusive_fullscreen(h: ExclusiveHints) -> bool {
    if h.dxgi_output_exclusive {
        return true;
    }
    h.foreground_covers_monitor && h.wgc_frame_near_black
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxgi_exclusive_flag_wins() {
        assert!(is_exclusive_fullscreen(ExclusiveHints {
            foreground_covers_monitor: false,
            dxgi_output_exclusive: true,
            wgc_frame_near_black: false,
        }));
    }

    #[test]
    fn covering_plus_black_wgc_is_exclusive() {
        assert!(is_exclusive_fullscreen(ExclusiveHints {
            foreground_covers_monitor: true,
            dxgi_output_exclusive: false,
            wgc_frame_near_black: true,
        }));
    }

    #[test]
    fn covering_alone_is_not_enough() {
        assert!(!is_exclusive_fullscreen(ExclusiveHints {
            foreground_covers_monitor: true,
            dxgi_output_exclusive: false,
            wgc_frame_near_black: false,
        }));
    }

    #[test]
    fn dxgi_detached_output_is_exclusive() {
        assert!(dxgi_output_is_exclusive(false, false));
        assert!(!dxgi_output_is_exclusive(true, false));
        assert!(dxgi_output_is_exclusive(true, true));
    }
}

//! CPU golden patches for SDR / HDR tonemap / mixed white-lift.
//! Shared helpers — do not reimplement HDR math elsewhere.

use crate::hdr::{tonemap_srgb8, white_level_adjust, REFERENCE_WHITE_NITS};

/// SDR UNORM byte → tonemap at `sdr_white` nits.
pub fn sdr_tonemap_channel(v: u8, sdr_white: f32) -> u8 {
    tonemap_srgb8(v as f32 / 255.0, sdr_white)
}

/// Scene-referred scRGB sample (1.0 = 80 nits) → tonemap.
pub fn hdr_scrgb_tonemap_channel(scrgb: f32, sdr_white: f32) -> u8 {
    tonemap_srgb8(scrgb, sdr_white)
}

/// Mixed-display path: lift SDR UNORM into scRGB at `sdr_white`, then tonemap back.
pub fn mixed_lift_tonemap_channel(sdr_unorm: u8, sdr_white: f32) -> u8 {
    let x = sdr_unorm as f32 / 255.0;
    let lifted = white_level_adjust(x, sdr_white, REFERENCE_WHITE_NITS);
    tonemap_srgb8(lifted, sdr_white)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_err(a: u8, b: u8) -> i16 {
        (a as i16 - b as i16).abs()
    }

    #[test]
    fn golden_single_sdr_channel_error_le_1() {
        let sdr_white = REFERENCE_WHITE_NITS;
        for v in 0..=255u8 {
            let got = sdr_tonemap_channel(v, sdr_white);
            let expected = tonemap_srgb8(v as f32 / 255.0, sdr_white);
            assert!(
                channel_err(got, expected) <= 1,
                "sdr v={v} got={got} expected={expected}"
            );
        }
    }

    #[test]
    fn golden_single_hdr_tonemap_channel_error_le_1() {
        let sdr_white = 203.0;
        // HDR scRGB samples around / above SDR white.
        let samples = [0.0_f32, 0.5, 1.0, 203.0 / 80.0, 2.5, 4.0];
        for scrgb in samples {
            let got = hdr_scrgb_tonemap_channel(scrgb, sdr_white);
            let expected = tonemap_srgb8(scrgb, sdr_white);
            assert!(
                channel_err(got, expected) <= 1,
                "hdr scrgb={scrgb} got={got} expected={expected}"
            );
        }
        assert_eq!(hdr_scrgb_tonemap_channel(203.0 / 80.0, 203.0), 255);
    }

    #[test]
    fn golden_mixed_lift_tonemap_channel_error_le_1() {
        let sdr_white = 203.0;
        for v in 0..=255u8 {
            let got = mixed_lift_tonemap_channel(v, sdr_white);
            let x = v as f32 / 255.0;
            let expected = tonemap_srgb8(x, REFERENCE_WHITE_NITS);
            assert!(
                channel_err(got, expected) <= 1,
                "mixed v={v} got={got} expected={expected}"
            );
        }
    }
}

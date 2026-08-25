//! White-level adjustment + sRGB OETF. Matches C# TonemapToSdr / SrgbOETFShader.
//!
//! WhiteLevelAdjustment: `rgb * (input_nits / output_nits)`
//! TonemapToSdr uses InputWhiteLevel=80, OutputWhiteLevel=sdr_white_nits, then OETF.

pub const REFERENCE_WHITE_NITS: f32 = 80.0;

/// Scale scene-referred scRGB (1.0 = 80 nits) so SDR white maps to 1.0.
pub fn white_level_adjust(sample: f32, input_nits: f32, output_nits: f32) -> f32 {
    let out = output_nits.max(1.0);
    sample * (input_nits / out)
}

pub fn tonemap_linear(scrgb: f32, sdr_white_nits: f32) -> f32 {
    white_level_adjust(scrgb, REFERENCE_WHITE_NITS, sdr_white_nits)
}

/// C# SrgbOETFShader: step at 0.0031308, 1.055 * x^(1/2.4) - 0.055 else * 12.92.
pub fn srgb_oetf(linear: f32) -> f32 {
    let x = linear.abs();
    if x <= 0.0031308 {
        linear * 12.92
    } else {
        let sign = if linear < 0.0 { -1.0 } else { 1.0 };
        sign * (1.055 * x.powf(1.0 / 2.4) - 0.055)
    }
}

pub fn tonemap_srgb8(scrgb: f32, sdr_white_nits: f32) -> u8 {
    let v = srgb_oetf(tonemap_linear(scrgb, sdr_white_nits)).clamp(0.0, 1.0);
    (v * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8
}

/// DISPLAYCONFIG_SDR_WHITE_LEVEL stores a multiplier of 80 nits × 1000.
/// 1000 => 80 nits. nits = value / 1000 * 80.
pub fn sdr_white_nits_from_displayconfig(sdr_white_level: u32) -> f32 {
    if sdr_white_level == 0 {
        REFERENCE_WHITE_NITS
    } else {
        (sdr_white_level as f32) / 1000.0 * REFERENCE_WHITE_NITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighty_nits_passthrough() {
        assert!((tonemap_linear(1.0, 80.0) - 1.0).abs() < 1e-6);
        assert_eq!(tonemap_srgb8(1.0, 80.0), 255);
        assert_eq!(tonemap_srgb8(0.0, 80.0), 0);
    }

    #[test]
    fn two_oh_three_nits_maps_sdr_white_to_one() {
        let scrgb_sdr_white = 203.0 / 80.0;
        let lin = tonemap_linear(scrgb_sdr_white, 203.0);
        assert!((lin - 1.0).abs() < 1e-5);
        assert_eq!(tonemap_srgb8(scrgb_sdr_white, 203.0), 255);
    }

    #[test]
    fn oetf_matches_piecewise_srgb() {
        let low = srgb_oetf(0.002);
        assert!((low - 0.002 * 12.92).abs() < 1e-6);
        let high = srgb_oetf(0.5);
        let expected = 1.055 * 0.5f32.powf(1.0 / 2.4) - 0.055;
        assert!((high - expected).abs() < 1e-6);
    }

    #[test]
    fn displayconfig_1000_is_80_nits() {
        assert!((sdr_white_nits_from_displayconfig(1000) - 80.0).abs() < 1e-4);
        assert!((sdr_white_nits_from_displayconfig(2537) - 203.0).abs() < 0.1);
    }
}

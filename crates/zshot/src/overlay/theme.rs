//! Overlay chrome tokens. Light screenshot toolbar; accent stays the brand blue.

use gpui::{hsla, rgb, Hsla};

pub fn base() -> Hsla {
    Hsla::from(rgb(0x191A1D))
}

pub fn surface() -> Hsla {
    Hsla::from(rgb(0xFFFFFF)).opacity(0.94)
}

pub fn elevated_surface() -> Hsla {
    Hsla::from(rgb(0xF3F4F6))
}

pub fn primary_fg() -> Hsla {
    Hsla::from(rgb(0x1C1D21))
}

pub fn secondary_fg() -> Hsla {
    Hsla::from(rgb(0x6B6F78))
}

pub fn disabled_fg() -> Hsla {
    Hsla::from(rgb(0x9AA0A8))
}

pub fn accent() -> Hsla {
    Hsla::from(rgb(0x548CFF))
}

pub fn accent_hover() -> Hsla {
    Hsla::from(rgb(0x6A9CFF))
}

pub fn accent_pressed() -> Hsla {
    Hsla::from(rgb(0x4278E8))
}

pub fn active_background() -> Hsla {
    Hsla::from(rgb(0x548CFF)).opacity(0.16)
}

pub fn border() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.06)
}

pub fn danger() -> Hsla {
    Hsla::from(rgb(0xFF5A52))
}

pub fn success() -> Hsla {
    Hsla::from(rgb(0x35C96F))
}

pub fn transparent() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.0)
}

/// ARGB for annotation strokes.
pub const STROKE_ARGB: u32 = 0xFFFA_3C3C;

/// (hex, packed RGB) — the annotation palette, shared by the settings page's
/// default-colour row and the overlay toolbar's colour popup so the two can
/// never offer different colours. The first entry is `STROKE_ARGB`.
pub const SWATCHES: [(&str, u32); 8] = [
    ("#FA3C3C", 0xFA3C3C),
    ("#FF8C00", 0xFF8C00),
    ("#FFD700", 0xFFD700),
    ("#22B14C", 0x22B14C),
    ("#3B82F6", 0x3B82F6),
    ("#A855F7", 0xA855F7),
    ("#1E1E1E", 0x1E1E1E),
    ("#FFFFFF", 0xFFFFFF),
];

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Rgba;

    fn rgb_of(c: Hsla) -> (u8, u8, u8) {
        let r = Rgba::from(c);
        (
            (r.r * 255.0).round() as u8,
            (r.g * 255.0).round() as u8,
            (r.b * 255.0).round() as u8,
        )
    }

    #[test]
    fn surface_and_washes_keep_specified_alpha() {
        assert!((surface().a - 0.94).abs() < 0.001);
        assert!((active_background().a - 0.16).abs() < 0.001);
        assert!((border().a - 0.06).abs() < 0.001);
        assert_eq!(border().h, 0.0);
        assert_eq!(border().s, 0.0);
        assert!(border().l < 0.01);
    }

    #[test]
    fn first_swatch_is_the_default_stroke() {
        assert_eq!(0xFF00_0000 | SWATCHES[0].1, STROKE_ARGB);
    }

    #[test]
    fn opaque_tokens_match_hex() {
        assert_eq!(rgb_of(base()), (0x19, 0x1A, 0x1D));
        assert_eq!(rgb_of(elevated_surface()), (0xF3, 0xF4, 0xF6));
        assert_eq!(rgb_of(primary_fg()), (0x1C, 0x1D, 0x21));
        assert_eq!(rgb_of(secondary_fg()), (0x6B, 0x6F, 0x78));
        assert_eq!(rgb_of(disabled_fg()), (0x9A, 0xA0, 0xA8));
        assert_eq!(rgb_of(accent()), (0x54, 0x8C, 0xFF));
        assert_eq!(rgb_of(accent_hover()), (0x6A, 0x9C, 0xFF));
        assert_eq!(rgb_of(accent_pressed()), (0x42, 0x78, 0xE8));
        assert_eq!(rgb_of(danger()), (0xFF, 0x5A, 0x52));
        assert_eq!(rgb_of(success()), (0x35, 0xC9, 0x6F));
        assert_eq!(rgb_of(Hsla::from(rgb(0xFFFFFF))), (0xFF, 0xFF, 0xFF));
    }
}

//! Near-black frame detector. Samples a grid so exclusive-fullscreen fallback
//! can kick in without a full-frame analysis.

const LUMA_R: u32 = 54;
const LUMA_G: u32 = 183;
const LUMA_B: u32 = 19;

/// `pixels` is BGRA8, `stride` in bytes. Returns true when the sampled mean
/// luma is below 4 and the max channel is below 16 (near-black / failed WGC).
pub fn is_near_black_bgra(pixels: &[u8], width: u32, height: u32, stride: usize) -> bool {
    if width == 0 || height == 0 {
        return true;
    }
    let step_x = ((width / 32).max(1)) as usize;
    let step_y = ((height / 32).max(1)) as usize;
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    let mut max_c: u8 = 0;
    let mut y = 0usize;
    while y < height as usize {
        let row = y * stride;
        let mut x = 0usize;
        while x < width as usize {
            let i = row + x * 4;
            if i + 3 < pixels.len() {
                let b = pixels[i];
                let g = pixels[i + 1];
                let r = pixels[i + 2];
                max_c = max_c.max(r).max(g).max(b);
                let yv = (LUMA_R * r as u32 + LUMA_G * g as u32 + LUMA_B * b as u32) / 256;
                sum += yv as u64;
                count += 1;
            }
            x += step_x;
        }
        y += step_y;
    }
    if count == 0 {
        return true;
    }
    let mean = sum / count;
    mean < 4 && max_c < 16
}

/// Sample FP16 scRGB (16-bit half per channel, RGBA). Used for HDR WGC frames
/// so a black exclusive-fullscreen miss is not skipped just because `hdr` is set.
pub fn is_near_black_fp16(pixels: &[u8], width: u32, height: u32, stride: usize) -> bool {
    if width == 0 || height == 0 {
        return true;
    }
    let step_x = ((width / 32).max(1)) as usize;
    let step_y = ((height / 32).max(1)) as usize;
    let mut max_abs: f32 = 0.0;
    let mut count: u64 = 0;
    let mut y = 0usize;
    while y < height as usize {
        let row = y * stride;
        let mut x = 0usize;
        while x < width as usize {
            let i = row + x * 8;
            if i + 7 < pixels.len() {
                let r = f16_to_f32(u16::from_le_bytes([pixels[i], pixels[i + 1]]));
                let g = f16_to_f32(u16::from_le_bytes([pixels[i + 2], pixels[i + 3]]));
                let b = f16_to_f32(u16::from_le_bytes([pixels[i + 4], pixels[i + 5]]));
                max_abs = max_abs.max(r.abs()).max(g.abs()).max(b.abs());
                count += 1;
            }
            x += step_x;
        }
        y += step_y;
    }
    count == 0 || max_abs < 0.02
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let fbits = if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            let mut m = mant;
            let mut e = 127 - 15;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= 0x3ff;
            (sign << 31) | ((e + 1) << 23) | (m << 13)
        }
    } else if exp == 31 {
        (sign << 31) | 0x7f800000 | (mant << 13)
    } else {
        (sign << 31) | ((exp + (127 - 15)) << 23) | (mant << 13)
    };
    f32::from_bits(fbits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_black_is_near_black() {
        let w = 64u32;
        let h = 64u32;
        let buf = vec![0u8; (w * h * 4) as usize];
        assert!(is_near_black_bgra(&buf, w, h, (w * 4) as usize));
    }

    #[test]
    fn bright_frame_is_not_near_black() {
        let w = 32u32;
        let h = 32u32;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for px in buf.as_chunks_mut::<4>().0 {
            px[0] = 40;
            px[1] = 80;
            px[2] = 120;
            px[3] = 255;
        }
        assert!(!is_near_black_bgra(&buf, w, h, (w * 4) as usize));
    }

    #[test]
    fn fp16_zeros_are_near_black() {
        let buf = vec![0u8; 8 * 8 * 8];
        assert!(is_near_black_fp16(&buf, 8, 8, 8 * 8));
    }

    #[test]
    fn fp16_one_is_not_near_black() {
        // f16 1.0 = 0x3C00
        let mut buf = vec![0u8; 8];
        buf[0] = 0x00;
        buf[1] = 0x3C;
        buf[2] = 0x00;
        buf[3] = 0x3C;
        buf[4] = 0x00;
        buf[5] = 0x3C;
        assert!(!is_near_black_fp16(&buf, 1, 1, 8));
    }
}

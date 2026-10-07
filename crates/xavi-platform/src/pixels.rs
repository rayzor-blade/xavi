use xavi_core::{Error, Result};

/// SDR BT.601 video-range NV12 for frameworks whose byte-buffer encoders accept
/// YUV only. Explicit color metadata is rejected by the public encoder profile.
pub fn nv12(bytes: &[u8], format: u32, width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let pixels = w
        .checked_mul(h)
        .filter(|p| *p <= super::MAX_BYTES / 4)
        .ok_or_else(|| Error::invalid("frame too large"))?;
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(Error::invalid("NV12 dimensions must be positive and even"));
    }
    if format == 3 && bytes.len() == pixels * 3 / 2 {
        return Ok(bytes.to_vec());
    }
    if format != 2 || bytes.len() != pixels * 4 {
        return Err(Error::invalid("invalid video input layout"));
    }
    let mut out = vec![0; pixels * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            let p = &bytes[(y * w + x) * 4..];
            let (b, g, r) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
            out[y * w + x] = (16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)).clamp(0, 255) as u8;
        }
    }
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut b, mut g, mut r) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = &bytes[((y + dy) * w + x + dx) * 4..];
                    b += i32::from(p[0]);
                    g += i32::from(p[1]);
                    r += i32::from(p[2]);
                }
            }
            let (b, g, r) = (b / 4, g / 4, r / 4);
            let index = pixels + y / 2 * w + x;
            out[index] = (128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)).clamp(0, 255) as u8;
            out[index + 1] = (128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)).clamp(0, 255) as u8;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn black_white_and_primary_colors_use_video_range_and_interleaved_chroma() {
        for (bgra, expected) in [
            ([0, 0, 0, 255], [16, 128, 128]),
            ([255, 255, 255, 255], [235, 128, 128]),
            ([0, 0, 255, 255], [82, 90, 240]),
            ([255, 0, 0, 255], [41, 240, 110]),
        ] {
            let bytes = nv12(&bgra.repeat(4), 2, 2, 2).unwrap();
            assert_eq!(&bytes[..4], &[expected[0]; 4]);
            assert_eq!(&bytes[4..], &expected[1..]);
            assert_eq!(nv12(&bytes, 3, 2, 2).unwrap(), bytes);
        }
        assert!(nv12(&[], 3, 0, 0).is_err());
        assert!(nv12(&[0; 12], 2, 1, 3).is_err());
        assert!(nv12(&[0; 15], 2, 2, 2).is_err());
        assert!(nv12(&[], 2, u32::MAX, u32::MAX).is_err());
    }
}

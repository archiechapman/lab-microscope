//! Packed YUV 4:2:2 to RGBA conversion (BT.601, full range, integer maths).

/// Byte order of a packed 4:2:2 pixel pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ByteOrder {
    /// U Y0 V Y1. What the device is documented to send.
    #[default]
    Uyvy,
    /// Y0 U Y1 V. Offered as a fallback in case the documentation is wrong.
    Yuyv,
}

#[inline]
fn clamp(x: i32) -> u8 {
    x.clamp(0, 255) as u8
}

/// Convert `src` (2 bytes per pixel) into `dst` (4 bytes per pixel, RGBA).
/// Converts as many whole pixel pairs as both buffers allow.
pub fn yuv422_to_rgba(src: &[u8], dst: &mut [u8], order: ByteOrder) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(8)) {
        let (y0, u, y1, v) = match order {
            ByteOrder::Uyvy => (s[1], s[0], s[3], s[2]),
            ByteOrder::Yuyv => (s[0], s[1], s[2], s[3]),
        };
        let u = u as i32 - 128;
        let v = v as i32 - 128;
        // 1.402, 0.344, 0.714, 1.772 scaled by 256.
        let dr = (359 * v) >> 8;
        let dg = (88 * u + 183 * v) >> 8;
        let db = (454 * u) >> 8;
        for (i, y) in [y0 as i32, y1 as i32].into_iter().enumerate() {
            let p = &mut d[i * 4..i * 4 + 4];
            p[0] = clamp(y + dr);
            p[1] = clamp(y - dg);
            p[2] = clamp(y + db);
            p[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_stays_grey() {
        let src = [128, 100, 128, 200];
        let mut dst = [0u8; 8];
        yuv422_to_rgba(&src, &mut dst, ByteOrder::Uyvy);
        assert_eq!(dst, [100, 100, 100, 255, 200, 200, 200, 255]);
    }

    #[test]
    fn byte_orders_agree() {
        let uyvy = [90, 120, 200, 60];
        let yuyv = [120, 90, 60, 200];
        let (mut a, mut b) = ([0u8; 8], [0u8; 8]);
        yuv422_to_rgba(&uyvy, &mut a, ByteOrder::Uyvy);
        yuv422_to_rgba(&yuyv, &mut b, ByteOrder::Yuyv);
        assert_eq!(a, b);
    }

    #[test]
    fn red_ish_chroma() {
        // High V pushes red up and green down.
        let mut dst = [0u8; 8];
        yuv422_to_rgba(&[128, 128, 240, 128], &mut dst, ByteOrder::Uyvy);
        assert!(dst[0] > 200 && dst[1] < 80);
    }
}

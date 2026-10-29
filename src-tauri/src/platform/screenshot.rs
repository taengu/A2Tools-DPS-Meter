//! Screenshots of the meter's own windows: to the clipboard, and optionally to
//! a PNG file.
//!
//! The page measures what to capture with `getBoundingClientRect`, which is in
//! CSS pixels relative to the window's client area. The screen is in physical
//! pixels, so the rect is scaled by the page's `devicePixelRatio` (Windows
//! display scaling, times any page zoom) and offset by where the client area
//! sits on screen. Skipping either crops or shifts the image at 125%/150%
//! scaling, which is what players reported.

/// A rectangle on screen, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

impl ScreenRect {
    /// The smallest rectangle covering both.
    pub fn union(self, other: ScreenRect) -> ScreenRect {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = (self.left as i64 + self.width as i64).max(other.left as i64 + other.width as i64);
        let bottom = (self.top as i64 + self.height as i64).max(other.top as i64 + other.height as i64);
        match (i32::try_from(right - left as i64), i32::try_from(bottom - top as i64)) {
            (Ok(width), Ok(height)) if self.width > 0 && self.height > 0 && other.width > 0 && other.height > 0 =>
                ScreenRect { left, top, width, height },
            _ => ScreenRect { left: 0, top: 0, width: 0, height: 0 },
        }
    }
}

/// A CSS-pixel rect inside a window's client area, as screen pixels, given the
/// client area's on-screen origin.
pub fn css_rect_to_screen(origin: (i32, i32), x: f64, y: f64, w: f64, h: f64, scale: f64) -> ScreenRect {
    let invalid = ScreenRect { left: 0, top: 0, width: 0, height: 0 };
    if ![x, y, w, h, scale].iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 || scale <= 0.0 {
        return invalid;
    }
    let coordinate = |origin: i32, css: f64| -> Option<i32> {
        let value = origin as f64 + (css * scale).round();
        (value.is_finite() && value >= i32::MIN as f64 && value <= i32::MAX as f64).then_some(value as i32)
    };
    let (Some(left), Some(top), Some(right), Some(bottom)) = (
        coordinate(origin.0, x), coordinate(origin.1, y), coordinate(origin.0, x + w), coordinate(origin.1, y + h)
    ) else { return invalid };
    match (right.checked_sub(left), bottom.checked_sub(top)) {
        (Some(width), Some(height)) if width > 0 && height > 0 => ScreenRect { left, top, width, height },
        _ => invalid,
    }
}

/// Encode top-down RGBA rows as a PNG. Hand-rolled because the only thing
/// needed is "write these pixels", and flate2 is already a dependency.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    use std::io::Write;

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }

    let stride = width as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks_exact(stride).take(height as usize) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = z.write_all(&raw);
    let idat = z.finish().unwrap_or_default();

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, deflate, no filter set, no interlace

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Capture, the default folder and the folder picker are the OS's.
pub use super::os::screen::{capture, default_folder, pick_folder};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_css_pixels_to_the_screen() {
        // A 300x200 CSS-pixel panel at (10, 20) in a window whose client area
        // starts at (1000, 500), on a 150% display.
        let r = css_rect_to_screen((1000, 500), 10.0, 20.0, 300.0, 200.0, 1.5);
        assert_eq!(r, ScreenRect { left: 1015, top: 530, width: 450, height: 300 });
        // At 100% it is a plain offset.
        let r = css_rect_to_screen((1000, 500), 10.0, 20.0, 300.0, 200.0, 1.0);
        assert_eq!(r, ScreenRect { left: 1010, top: 520, width: 300, height: 200 });
    }

    #[test]
    fn union_covers_both_windows() {
        let meter = ScreenRect { left: 100, top: 100, width: 400, height: 300 };
        let details = ScreenRect { left: 520, top: 80, width: 800, height: 600 };
        assert_eq!(meter.union(details), ScreenRect { left: 100, top: 80, width: 1220, height: 600 });
    }

    #[test]
    fn invalid_or_overflowing_capture_rectangles_are_empty() {
        assert_eq!(css_rect_to_screen((i32::MAX, 0), 1.0, 0.0, 100.0, 100.0, 1.0).width, 0);
        assert_eq!(css_rect_to_screen((0, 0), f64::INFINITY, 0.0, 100.0, 100.0, 1.0).width, 0);
        assert_eq!(css_rect_to_screen((0, 0), 0.0, 0.0, f64::MAX, 100.0, 2.0).width, 0);
        assert_eq!(css_rect_to_screen((0, 0), 0.0, 0.0, 100.0, 100.0, f64::NAN).width, 0);
        let left = ScreenRect { left: i32::MIN, top: 0, width: 100, height: 100 };
        let right = ScreenRect { left: i32::MAX - 100, top: 0, width: 100, height: 100 };
        assert_eq!(left.union(right).width, 0);
    }

    #[test]
    fn png_is_well_formed() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // CRC of an empty IEND chunk is a fixed, well-known value.
        assert_eq!(&png[png.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
    }
}

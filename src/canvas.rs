//! The 8-bit grayscale drawing surface and its 4bpp wire encoding.

use std::io;
use std::time::Duration;

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 64;

/// Encoded bytes sent to the panel for one frame.
pub const WIRE_FRAME_BYTES: usize = 8704;

/// Raw grayscale bytes for one external frame: one byte per pixel.
pub const GRAYSCALE_BYTES: usize = WIDTH * HEIGHT;

const PACKET_BYTES: usize = 512;

// Pixel bytes consumed by one packet: 508 payload bytes at 4bpp.
const PIXELS_PER_PACKET: usize = (PACKET_BYTES - 4) * 2;

/// Fade rates are expressed per frame at this reference rate, so a `trail`
/// option keeps the same look regardless of the configured playback rate.
const REFERENCE_FPS: f64 = 30.0;

/// 8-bit grayscale surface.
///
/// Only the top nibble of each pixel is sent to the panel, but keeping 8 bits
/// here makes fades and antialiasing smooth.
pub struct Canvas {
    pixels: Box<[u8]>,
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; WIDTH * HEIGHT].into_boxed_slice(),
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Copy a raw row-major grayscale frame into the canvas.
    ///
    /// This is the contract used by external frame producers: exactly
    /// [`WIDTH`] × [`HEIGHT`] bytes, one byte per pixel.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::InvalidData`] when the frame size is wrong.
    pub fn set_grayscale(&mut self, pixels: &[u8]) -> io::Result<()> {
        if pixels.len() != self.pixels.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected a {}x{} grayscale frame ({} bytes), got {} bytes",
                    WIDTH,
                    HEIGHT,
                    self.pixels.len(),
                    pixels.len()
                ),
            ));
        }
        self.pixels.copy_from_slice(pixels);
        Ok(())
    }

    /// Fade every pixel so trails decay at a fixed rate per second.
    ///
    /// `keep_percent` is the fraction retained by one reference frame, which
    /// keeps the visual look stable when `dt` varies.
    pub fn fade(&mut self, keep_percent: u8, dt: Duration) {
        let keep = f64::from(keep_percent.min(100)) / 100.0;
        let decay = keep.powf(dt.as_secs_f64() * REFERENCE_FPS);

        if decay >= 1.0 {
            return;
        }
        if decay <= 0.0 {
            self.clear();
            return;
        }

        let mut table = [0u8; 256];
        for (value, faded) in table.iter_mut().enumerate() {
            *faded = (value as f64 * decay + 0.5) as u8;
        }
        for pixel in &mut self.pixels {
            *pixel = table[*pixel as usize];
        }
    }

    /// Brighten one pixel. Blend is max so overlapping shapes do not flicker.
    pub fn plot(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || x >= WIDTH as i32 || y < 0 || y >= HEIGHT as i32 {
            return;
        }

        let idx = y as usize * WIDTH + x as usize;
        if value > self.pixels[idx] {
            self.pixels[idx] = value;
        }
    }

    /// Pack 4bpp pixels directly into the packet payloads.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::InvalidData`] unless `out` is a whole number of
    /// panel packets.
    pub fn encode(&self, out: &mut [u8]) -> io::Result<()> {
        if !out.len().is_multiple_of(PACKET_BYTES) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame buffer must be a multiple of {PACKET_BYTES} bytes"),
            ));
        }

        let packets = out.len() / PACKET_BYTES;
        for (page, packet) in out.as_chunks_mut::<PACKET_BYTES>().0.iter_mut().enumerate() {
            packet[0] = page as u8;
            packet[1] = u8::from(page == packets - 1);

            let start = page * PIXELS_PER_PACKET;
            let end = (start + PIXELS_PER_PACKET).min(self.pixels.len());
            let src = &self.pixels[start..end];
            let payload = packet[4..].as_chunks_mut::<4>().0;

            // 8 pixels -> 4 bytes; the top nibble of each pixel is its 4bpp value.
            for (px, quad) in src.as_chunks::<8>().0.iter().zip(payload) {
                quad[0] = (px[2] >> 4) | (px[3] & 0xf0);
                quad[1] = (px[0] >> 4) | (px[1] & 0xf0);
                quad[2] = (px[6] >> 4) | (px[7] & 0xf0);
                quad[3] = (px[4] >> 4) | (px[5] & 0xf0);
            }
        }
        Ok(())
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_packs_pixels_and_marks_last_packet() {
        let mut canvas = Canvas::new();
        canvas.plot(0, 0, 0xf0);

        let mut frame = vec![0u8; WIRE_FRAME_BYTES];
        canvas.encode(&mut frame).unwrap();

        assert_eq!(frame[5], 0x0f);
        assert_eq!(frame[1], 0);

        let last = frame.len() - PACKET_BYTES;
        assert_eq!(frame[last], 16);
        assert_eq!(frame[last + 1], 1);
    }

    #[test]
    fn fade_and_clear_apply_to_pixels() {
        let mut canvas = Canvas::new();
        canvas.plot(0, 0, 255);
        canvas.fade(66, Duration::from_secs_f64(1.0 / REFERENCE_FPS));

        let mut frame = vec![0u8; WIRE_FRAME_BYTES];
        canvas.encode(&mut frame).unwrap();
        assert_eq!(frame[5], 0x0a); // 255 * 0.66 = 168, high nibble

        canvas.clear();
        canvas.encode(&mut frame).unwrap();
        assert_eq!(frame[5], 0);
    }

    #[test]
    fn fade_is_rate_independent() {
        let mut at_30 = Canvas::new();
        let mut at_60 = Canvas::new();
        at_30.plot(0, 0, 255);
        at_60.plot(0, 0, 255);

        at_30.fade(66, Duration::from_secs_f64(1.0 / 30.0));
        for _ in 0..2 {
            at_60.fade(66, Duration::from_secs_f64(1.0 / 60.0));
        }

        let a = at_30.pixels()[0];
        let b = at_60.pixels()[0];
        assert!(
            a.abs_diff(b) <= 1,
            "30 fps fade {a} should match 60 fps fade {b}"
        );
    }

    #[test]
    fn grayscale_frames_validate_their_size() {
        let mut canvas = Canvas::new();
        let frame = vec![7u8; WIDTH * HEIGHT];
        canvas.set_grayscale(&frame).unwrap();
        assert!(canvas.pixels().iter().all(|&p| p == 7));

        assert!(canvas.set_grayscale(&frame[..10]).is_err());
    }

    #[test]
    fn encode_rejects_partial_packets() {
        let canvas = Canvas::new();
        let mut frame = vec![0u8; WIRE_FRAME_BYTES - 1];
        assert!(canvas.encode(&mut frame).is_err());
    }
}

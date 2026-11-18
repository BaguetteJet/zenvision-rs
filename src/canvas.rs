pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 64;

const PACKET_BYTES: usize = 512;

// Pixel bytes consumed by one packet: 508 payload bytes at 4bpp.
const PIXELS_PER_PACKET: usize = (PACKET_BYTES - 4) * 2;

// 8-bit grayscale surface. Only the top nibble of each pixel is sent to the panel, but keeping 8 bits here makes fades and antialiasing smooth.
pub struct Canvas {
    pixels: Vec<u8>,
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; WIDTH * HEIGHT],
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    // Fade every pixel by `keep_percent`, so trails decay at a fixed rate.
    pub fn fade(&mut self, keep_percent: u8) {
        assert!(keep_percent <= 100, "keep_percent must be 0-100");

        let mut decay = [0u8; 256];
        for (value, decayed) in decay.iter_mut().enumerate() {
            *decayed = (value as u16 * keep_percent as u16 / 100) as u8;
        }

        for pixel in &mut self.pixels {
            *pixel = decay[*pixel as usize];
        }
    }

    // Brighten one pixel. Blend is max so overlapping shapes do not flicker.
    pub fn plot(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || x >= WIDTH as i32 || y < 0 || y >= HEIGHT as i32 {
            return;
        }

        let idx = y as usize * WIDTH + x as usize;
        if value > self.pixels[idx] {
            self.pixels[idx] = value;
        }
    }

    // Pack 4bpp pixels directly into the packet payloads, with no staging buffer.
    pub fn encode(&self, out: &mut [u8]) {
        assert_eq!(
            self.pixels.len(),
            WIDTH * HEIGHT,
            "expected {} pixels",
            WIDTH * HEIGHT
        );
        assert_eq!(out.len() % PACKET_BYTES, 0, "invalid frame size");

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
    use crate::device::FRAME_BYTES;

    #[test]
    fn encode_packs_pixels_and_marks_last_packet() {
        let mut canvas = Canvas::new();
        canvas.plot(0, 0, 0xf0);

        let mut frame = vec![0u8; FRAME_BYTES];
        canvas.encode(&mut frame);

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
        canvas.fade(66);

        let mut frame = vec![0u8; FRAME_BYTES];
        canvas.encode(&mut frame);
        assert_eq!(frame[5], 0x0a); // 255 * 66 / 100 = 168, high nibble

        canvas.clear();
        canvas.encode(&mut frame);
        assert_eq!(frame[5], 0);
    }
}

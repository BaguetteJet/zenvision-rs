use std::io;
use std::thread;
use std::time::{Duration, Instant};

use nusb::transfer::Buffer;

use crate::commands;
use crate::usb::{Dev, FRAME_BYTES, send_frame};

const WIDTH: usize = 256;
const HEIGHT: usize = 64;
const PACKET_BYTES: usize = 512;
const PACKETS: usize = 17;

// Pixel bytes consumed by one 512-byte packet: 508 payload bytes at 4bpp.
const PIXELS_PER_PAGE: usize = (PACKET_BYTES - 4) * 2;

const STARS: usize = 90;
const TRAILS: u16 = 66;

// Exact per-pixel trail decay, precomputed to keep the hot loop to one lookup.
static DECAY: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = (i as u16 * TRAILS / 100) as u8;
        i += 1;
    }
    table
};

// Stream the starfield until the process is interrupted.
pub fn run(dev: &mut Dev, fps: u32) -> io::Result<()> {
    println!("streaming starfield at {fps} fps");
    start(dev)?;
    run_with(dev, fps, |_| Ok(true))
}

// Hand the panel over to host frames (content mode 2, sweep off).
pub fn start(dev: &mut Dev) -> io::Result<()> {
    commands::set_content_mode(dev, 2)?;
    commands::set_screen_sweep(dev, false)
}

// Stream frames, asking `tick` before every send whether the host still owns
// the panel. Used by the daemon to interleave commands with frames.
pub fn run_with<F>(dev: &mut Dev, fps: u32, mut tick: F) -> io::Result<()>
where
    F: FnMut(&mut Dev) -> io::Result<bool>,
{
    assert!(fps > 0, "fps must be greater than zero");

    let period = Duration::from_secs_f64(1.0 / fps as f64);
    let mut stars = Starfield::new(fps);
    let mut frame = Buffer::from(vec![0u8; FRAME_BYTES]);
    let mut deadline = Instant::now();

    loop {
        if tick(dev)? {
            stars.draw(&mut frame);
            frame = send_frame(dev, frame)?;
        }

        // Sleep until the next frame is due so transfer time does not lower the
        // frame rate; if we fell behind, restart the schedule instead of bursting.
        deadline += period;
        let now = Instant::now();
        if let Some(wait) = deadline.checked_duration_since(now) {
            thread::sleep(wait);
        } else {
            deadline = now;
        }
    }
}

struct Star {
    x: f32,
    y: f32,
    z: f32,
    tiny: bool,
}

struct Starfield {
    stars: Vec<Star>,
    trail: Vec<u8>,
    lit: Vec<u16>, // indices of non-zero trail pixels
    rng: Rng,
    speed: f32,
}

impl Starfield {
    fn new(fps: u32) -> Self {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut stars = Vec::with_capacity(STARS);
        for _ in 0..STARS {
            stars.push(Star {
                x: rng.range(-1.0, 1.0),
                y: rng.range(-1.0, 1.0),
                z: rng.range(0.05, 1.0),
                tiny: rng.chance(0.7),
            });
        }

        // Audio is off, so the reference applet uses a constant 0.1 level.
        let dt = (1.0 / fps as f32).min(0.1);
        let speed = (0.25 + 1.4 * 0.1) * dt;

        Self {
            stars,
            trail: vec![0; WIDTH * HEIGHT],
            lit: Vec::new(),
            rng,
            speed,
        }
    }

    // Render one frame straight into the wire format: no intermediate image.
    fn draw(&mut self, out: &mut [u8]) {
        assert_eq!(out.len(), FRAME_BYTES, "invalid frame size");

        let speed = self.speed;
        let rng = &mut self.rng;
        for star in &mut self.stars {
            star.z -= speed;
            if star.z <= 0.02 {
                star.x = rng.range(-1.0, 1.0);
                star.y = rng.range(-1.0, 1.0);
                star.z = rng.range(0.9, 1.0);
                star.tiny = rng.chance(0.7);
            }
        }

        // Decay only the pixels that are still lit; zero stays zero.
        let Self {
            stars, trail, lit, ..
        } = self;
        let mut i = 0;
        while i < lit.len() {
            let idx = lit[i] as usize;
            let value = DECAY[trail[idx] as usize];
            trail[idx] = value;
            if value == 0 {
                lit.swap_remove(i);
            } else {
                i += 1;
            }
        }

        let scale = HEIGHT as f32 * 0.9;
        let cx = WIDTH as f32 / 2.0;
        let cy = HEIGHT as f32 / 2.0;

        for star in stars {
            let sx = cx + star.x / star.z * scale;
            let sy = cy + star.y / star.z * scale;
            let ix = sx.floor() as i32;
            let iy = sy.floor() as i32;
            if ix < 0 || ix >= WIDTH as i32 || iy < 0 || iy >= HEIGHT as i32 {
                continue;
            }

            let g = (255.0 * (1.0 - star.z)) as u8;
            let idx = iy as usize * WIDTH + ix as usize;
            plot(trail, lit, idx, g);

            // Near stars bloom into 2x2 blocks; the rest stay single pixels.
            if !star.tiny && star.z <= 0.4 {
                if ix + 1 < WIDTH as i32 {
                    plot(trail, lit, idx + 1, g);
                }
                if iy + 1 < HEIGHT as i32 {
                    plot(trail, lit, idx + WIDTH, g);
                }
                if ix + 1 < WIDTH as i32 && iy + 1 < HEIGHT as i32 {
                    plot(trail, lit, idx + WIDTH + 1, g);
                }
            }
        }

        encode(trail, out);
    }
}

// Brighten one pixel, recording it when it was dark so `lit` stays exact.
fn plot(trail: &mut [u8], lit: &mut Vec<u16>, idx: usize, value: u8) {
    if value > trail[idx] {
        if trail[idx] == 0 {
            lit.push(idx as u16);
        }
        trail[idx] = value;
    }
}

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }

    fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

// Pack 4bpp pixels directly into the packet payloads, with no staging buffer.
fn encode(pixels: &[u8], out: &mut [u8]) {
    assert_eq!(pixels.len(), WIDTH * HEIGHT, "expected 256x64 pixels");
    assert_eq!(out.len(), FRAME_BYTES, "invalid frame size");

    for (page, packet) in out.as_chunks_mut::<PACKET_BYTES>().0.iter_mut().enumerate() {
        packet[0] = page as u8;
        packet[1] = u8::from(page == PACKETS - 1);

        let start = page * PIXELS_PER_PAGE;
        let src = &pixels[start..(start + PIXELS_PER_PAGE).min(pixels.len())];
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

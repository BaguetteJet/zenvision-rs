use std::time::Duration;

use super::Animation;
use crate::canvas::{Canvas, HEIGHT, WIDTH};

const DEFAULT_STARS: usize = 90;
const DEFAULT_TRAIL: u8 = 66;

#[derive(Clone, Debug, PartialEq, clap::Args)]
pub struct Options {
    /// Number of stars
    #[arg(long, default_value_t = DEFAULT_STARS)]
    stars: usize,
    /// Trail pixels percentage
    #[arg(
        long, default_value_t = DEFAULT_TRAIL,
        value_parser = clap::value_parser!(u8).range(0..=100)
    )]
    trail: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stars: DEFAULT_STARS,
            trail: DEFAULT_TRAIL,
        }
    }
}

pub struct Starfield {
    stars: Vec<Star>,
    rng: Rng,
    trail: u8,
}

struct Star {
    x: f32,
    y: f32,
    z: f32,
    tiny: bool,
}

impl Starfield {
    pub fn new(options: Options) -> Self {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut stars = Vec::with_capacity(options.stars);
        for _ in 0..options.stars {
            stars.push(Star {
                x: rng.range(-1.0, 1.0),
                y: rng.range(-1.0, 1.0),
                z: rng.range(0.05, 1.0),
                tiny: rng.chance(0.7),
            });
        }

        Self {
            stars,
            rng,
            trail: options.trail,
        }
    }
}

impl Animation for Starfield {
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration) {
        // Audio is off, so the reference applet uses a constant 0.1 level.
        let dt = dt.as_secs_f32().min(0.1);
        let speed = (0.25 + 1.4 * 0.1) * dt;

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

        canvas.fade(self.trail);

        let scale = HEIGHT as f32 * 0.9;
        let cx = WIDTH as f32 / 2.0;
        let cy = HEIGHT as f32 / 2.0;

        for star in &self.stars {
            let sx = (cx + star.x / star.z * scale).floor() as i32;
            let sy = (cy + star.y / star.z * scale).floor() as i32;
            if sx < 0 || sx >= WIDTH as i32 || sy < 0 || sy >= HEIGHT as i32 {
                continue;
            }

            let g = (255.0 * (1.0 - star.z)) as u8;
            canvas.plot(sx, sy, g);

            // Near stars bloom into 2x2 blocks.
            if !star.tiny && star.z <= 0.4 {
                canvas.plot(sx + 1, sy, g);
                canvas.plot(sx, sy + 1, g);
                canvas.plot(sx + 1, sy + 1, g);
            }
        }
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

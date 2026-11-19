//! Warp-speed starfield.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Animation, Status};
use crate::canvas::{Canvas, HEIGHT, WIDTH};
use crate::protocol::SpeedMultiplier;

const DEFAULT_STARS: u32 = 90;
const DEFAULT_TRAIL: u8 = 66;
const MAX_STARS: u32 = 100_000;

// Projection scale used by the renderer below. Keeping the spawn frustum in
// sync with it guarantees that every star is visible when it appears.
const SCALE: f32 = HEIGHT as f32 * 0.9;
const X_AT_UNIT_Z: f32 = (WIDTH as f32 * 0.5) / SCALE;
const Y_AT_UNIT_Z: f32 = (HEIGHT as f32 * 0.5) / SCALE;
const SPAWN_MARGIN: f32 = 0.98;

/// Starfield tuning, shared by the CLI, IPC and the state file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Number of stars.
    pub stars: u32,
    /// Speed multiplier.
    pub speed: SpeedMultiplier,
    /// Trail retention percentage per reference frame (0-100).
    pub trail: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stars: DEFAULT_STARS,
            speed: SpeedMultiplier::DEFAULT,
            trail: DEFAULT_TRAIL,
        }
    }
}

pub struct Starfield {
    stars: Vec<Star>,
    rng: Rng,
    speed: f32,
    trail: u8,
}

impl Starfield {
    pub fn new(options: Options) -> Self {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut stars = Vec::with_capacity(options.stars.min(MAX_STARS) as usize);
        for _ in 0..options.stars.min(MAX_STARS) {
            let mut star = Star {
                x: 0.0,
                y: 0.0,
                z: 1.0,
                tiny: false,
            };
            let z = rng.range(0.05, 1.0);
            spawn(&mut rng, &mut star, z);
            stars.push(star);
        }

        Self {
            stars,
            rng,
            speed: options.speed.get(),
            trail: options.trail,
        }
    }
}

impl Animation for Starfield {
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration) -> io::Result<Status> {
        // Audio is off, so the reference applet uses a constant 0.1 level.
        let dt = dt.as_secs_f32().min(0.1);
        let speed = (0.25 + 1.4 * 0.1) * self.speed * dt;

        let rng = &mut self.rng;
        for star in &mut self.stars {
            star.z -= speed;
            if star.z <= 0.02 {
                let z = rng.range(0.9, 1.0);
                spawn(rng, star, z);
            }
        }

        canvas.fade(self.trail, Duration::from_secs_f32(dt));

        let cx = WIDTH as f32 / 2.0;
        let cy = HEIGHT as f32 / 2.0;

        for star in &self.stars {
            let sx = (cx + star.x / star.z * SCALE).floor() as i32;
            let sy = (cy + star.y / star.z * SCALE).floor() as i32;
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

        Ok(Status::New)
    }
}

struct Star {
    x: f32,
    y: f32,
    z: f32,
    tiny: bool,
}

/// Place a star inside the visible frustum at depth `z`.
///
/// The panel only shows `|y / z| < HEIGHT / (2 * SCALE)`, so spawning `y` in
/// `-1..1` leaves roughly half the stars permanently off-screen as they move
/// outward. Scaling both axes by the frustum at `z` makes every star visible
/// when it appears while preserving the projection and speed.
fn spawn(rng: &mut Rng, star: &mut Star, z: f32) {
    star.x = rng.range(-1.0, 1.0) * (X_AT_UNIT_Z * z * SPAWN_MARGIN).min(1.0);
    star.y = rng.range(-1.0, 1.0) * Y_AT_UNIT_Z * z * SPAWN_MARGIN;
    star.z = z;
    star.tiny = rng.chance(0.7);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn on_screen(star: &Star) -> bool {
        let cx = WIDTH as f32 / 2.0;
        let cy = HEIGHT as f32 / 2.0;
        let sx = (cx + star.x / star.z * SCALE).floor();
        let sy = (cy + star.y / star.z * SCALE).floor();
        (0.0..WIDTH as f32).contains(&sx) && (0.0..HEIGHT as f32).contains(&sy)
    }

    #[test]
    fn every_star_spawns_inside_the_visible_frustum() {
        let field = Starfield::new(Options {
            stars: 5_000,
            ..Options::default()
        });

        let visible = field.stars.iter().filter(|star| on_screen(star)).count();
        assert_eq!(visible, field.stars.len());
    }

    #[test]
    fn respawned_stars_are_visible_too() {
        let mut rng = Rng(42);

        for _ in 0..5_000 {
            let mut star = Star {
                x: 0.0,
                y: 0.0,
                z: 1.0,
                tiny: false,
            };
            let z = rng.range(0.9, 1.0);
            spawn(&mut rng, &mut star, z);
            assert!(on_screen(&star));
        }
    }
}

//! Falling glyph rain.
//!
//! The glyphs are a fixed 9x10 bitmap atlas, so the animation has no font or
//! rasterizer dependency. Columns are independent; each one falls at a constant
//! speed and fades from a bright head toward the tail. The whole field also
//! drifts a few pixels every minute to spread wear on OLED panels.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Animation, Rng, Status};
use crate::canvas::{Canvas, HEIGHT, WIDTH};

mod glyphs;

pub const DEFAULT_SIZE: u8 = 9;
pub const MIN_SIZE: u8 = 9;
pub const MAX_SIZE: u8 = 16;
pub const DEFAULT_SPEED: u16 = 150;
pub const MIN_SPEED: u16 = 20;
pub const MAX_SPEED: u16 = 400;

/// The Python applet uses this level when audio reactivity is disabled.
const IDLE_LEVEL: f32 = 0.4;

/// How long the field stays at one horizontal offset before drifting.
const DRIFT_INTERVAL: f32 = 60.0;

/// Selectable glyph sets for the rain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum GlyphSet {
    /// Katakana, halfwidth katakana and digits
    #[default]
    Katakana,
    /// Binary digits
    Binary,
    /// PlayStation controller symbols
    Playstation,
    /// Mathematical operators and symbols
    Maths,
    /// Printable ASCII
    Ascii,
    /// Minecraft enchantment runes
    Minecraft,
}

/// Matrix rain tuning, shared by the CLI, IPC and the state file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, clap::Args)]
#[serde(default)]
pub struct Options {
    /// Glyph set
    #[arg(long, value_enum, default_value = "katakana")]
    pub glyphs: GlyphSet,
    /// Cell spacing in pixels (larger is sparser)
    #[arg(
        long,
        default_value_t = DEFAULT_SIZE,
        value_parser = clap::value_parser!(u8).range(i64::from(MIN_SIZE)..=i64::from(MAX_SIZE))
    )]
    pub size: u8,
    /// Speed percentage
    #[arg(
        long,
        default_value_t = DEFAULT_SPEED,
        value_parser = clap::value_parser!(u16).range(i64::from(MIN_SPEED)..=i64::from(MAX_SPEED))
    )]
    pub speed: u16,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            glyphs: GlyphSet::Katakana,
            size: DEFAULT_SIZE,
            speed: DEFAULT_SPEED,
        }
    }
}

pub struct Matrix {
    heads: Vec<f32>,
    levels: Vec<u8>,
    rng: Rng,
    cell: usize,
    speed: f32,
    atlas: &'static [[u16; glyphs::CELL_HEIGHT]],
    offset: i32,
    max_offset: i32,
    drift_direction: i32,
    drift_elapsed: f32,
}

impl Matrix {
    pub fn new(options: Options) -> Self {
        let cell = usize::from(options.size.clamp(MIN_SIZE, MAX_SIZE));
        let speed = cell as f32
            * (0.6 + 1.8 * IDLE_LEVEL)
            * f32::from(options.speed.clamp(MIN_SPEED, MAX_SPEED))
            / 100.0;
        let tail = HEIGHT / cell + 3;

        let mut rng = Rng(0xd1b5_4a32_d192_ed03);
        // Give up one column so the whole field can drift across a useful
        // margin without clipping glyphs at either edge.
        let columns = (WIDTH / cell).saturating_sub(1);
        let heads = (0..columns)
            .map(|_| rng.range(-(HEIGHT as f32), 0.0))
            .collect();
        let levels = (0..tail).map(|index| fade_level(index, tail)).collect();
        let atlas = match options.glyphs {
            GlyphSet::Katakana => glyphs::KATAKANA,
            GlyphSet::Binary => glyphs::BINARY,
            GlyphSet::Playstation => glyphs::PLAYSTATION,
            GlyphSet::Maths => glyphs::MATHS,
            GlyphSet::Ascii => glyphs::ASCII,
            GlyphSet::Minecraft => glyphs::MINECRAFT,
        };
        let max_offset = (WIDTH - (columns - 1) * cell - glyphs::CELL_WIDTH) as i32;
        let offset = max_offset / 2;

        Self {
            heads,
            levels,
            rng,
            cell,
            speed,
            atlas,
            offset,
            max_offset,
            drift_direction: 1,
            drift_elapsed: 0.0,
        }
    }

    /// Nudge every column sideways to minimize OLED burn-in. Moving in small
    /// steps and reversing at the edges keeps the motion from reading as a jump.
    fn drift(&mut self) {
        if self.max_offset == 0 {
            return;
        }

        let step = 1 + (self.rng.next_u64() % 2) as i32;
        let mut next = self.offset + self.drift_direction * step;
        if !(0..=self.max_offset).contains(&next) {
            self.drift_direction = -self.drift_direction;
            next = self.offset + self.drift_direction * step;
        }
        self.offset = next.clamp(0, self.max_offset);
    }
}

impl Animation for Matrix {
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration) -> io::Result<Status> {
        canvas.clear();

        let elapsed = dt.as_secs_f32();
        let dt = elapsed.min(0.1);
        let step = self.cell as f32;
        let height = HEIGHT as f32;
        let tail = self.levels.len();

        self.drift_elapsed += elapsed;
        if self.drift_elapsed >= DRIFT_INTERVAL {
            self.drift_elapsed -= DRIFT_INTERVAL;
            self.drift();
        }

        for (column, head) in self.heads.iter_mut().enumerate() {
            *head += self.speed * dt;
            if *head - tail as f32 * step > height {
                *head = self.rng.range(-height * 0.5, 0.0);
            }

            let x = (column * self.cell) as i32 + self.offset;
            for (index, &level) in self.levels.iter().enumerate() {
                let y = *head - index as f32 * step;
                if y <= -(glyphs::CELL_HEIGHT as f32) || y >= height {
                    continue;
                }

                let glyph = glyph_index(column, (y / step).floor() as i32, self.atlas.len());
                draw_glyph(canvas, x, y.floor() as i32, &self.atlas[glyph], level);
            }
        }

        Ok(Status::New)
    }
}

/// Bright head, fading tail. The tail always keeps at least a little glow.
fn fade_level(index: usize, tail: usize) -> u8 {
    if index == 0 {
        255
    } else {
        (220 - index * 200 / tail).max(30) as u8
    }
}

fn draw_glyph(canvas: &mut Canvas, x: i32, y: i32, glyph: &[u16; glyphs::CELL_HEIGHT], level: u8) {
    for (row, bits) in glyph.iter().enumerate() {
        let py = y + row as i32;
        if py < 0 || py >= HEIGHT as i32 {
            continue;
        }

        for column in 0..glyphs::CELL_WIDTH {
            if bits & (1 << column) != 0 {
                canvas.plot(x + column as i32, py, level);
            }
        }
    }
}

/// Pick a glyph from the column and the screen cell. Keeping the choice tied
/// to the cell makes characters stable as they fall instead of rerolling every
/// frame, which is cheaper and much less noisy.
fn glyph_index(column: usize, cell: i32, count: usize) -> usize {
    let mut value = (column as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (cell as i64 as u64).wrapping_mul(0xd6e8_feb8_6659_fd93)
        ^ 0xa076_1d64_78bd_642f;

    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;

    (value % count as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_a_non_empty_frame() {
        let mut matrix = Matrix::new(Options::default());
        let mut canvas = Canvas::new();

        matrix.draw(&mut canvas, Duration::from_millis(100)).unwrap();

        assert!(canvas.pixels().iter().any(|&pixel| pixel != 0));
    }

    #[test]
    fn identical_options_draw_identical_frames() {
        let mut first = Matrix::new(Options::default());
        let mut second = Matrix::new(Options::default());
        let mut first_canvas = Canvas::new();
        let mut second_canvas = Canvas::new();

        for _ in 0..50 {
            first
                .draw(&mut first_canvas, Duration::from_millis(41))
                .unwrap();
            second
                .draw(&mut second_canvas, Duration::from_millis(41))
                .unwrap();
        }

        assert_eq!(first_canvas.pixels(), second_canvas.pixels());
    }

    #[test]
    fn horizontal_drift_stays_inside_the_canvas() {
        for size in MIN_SIZE..=MAX_SIZE {
            let mut matrix = Matrix::new(Options {
                size,
                ..Options::default()
            });
            let mut canvas = Canvas::new();
            let mut offsets = std::collections::BTreeSet::new();

            assert!(
                matrix.max_offset >= matrix.cell as i32,
                "size {size} drift is narrower than one cell"
            );
            assert_eq!(matrix.offset, matrix.max_offset / 2);

            for _ in 0..100 {
                matrix.drift_elapsed = DRIFT_INTERVAL;
                matrix.draw(&mut canvas, Duration::ZERO).unwrap();
                offsets.insert(matrix.offset);

                let right = (matrix.heads.len() - 1) * matrix.cell
                    + glyphs::CELL_WIDTH
                    + matrix.offset as usize;
                assert!(right <= WIDTH, "size {size}, offset {}", matrix.offset);
            }

            assert!(offsets.len() > 1, "size {size} never drifted");
        }
    }

    #[test]
    fn options_round_trip_through_json() {
        let options = Options {
            glyphs: GlyphSet::Binary,
            size: 12,
            speed: 220,
        };
        let encoded = serde_json::to_string(&options).unwrap();

        assert_eq!(serde_json::from_str::<Options>(&encoded).unwrap(), options);
    }

    #[test]
    fn glyph_sets_use_the_expected_atlas() {
        let cases = [
            (GlyphSet::Katakana, 61),
            (GlyphSet::Binary, 2),
            (GlyphSet::Playstation, 4),
            (GlyphSet::Maths, 33),
            (GlyphSet::Ascii, 94),
            (GlyphSet::Minecraft, 26),
        ];

        for (glyphs, count) in cases {
            let matrix = Matrix::new(Options {
                glyphs,
                ..Options::default()
            });
            assert_eq!(matrix.atlas.len(), count);
            assert!(
                matrix
                    .atlas
                    .iter()
                    .all(|glyph| glyph.iter().any(|&row| row != 0)),
                "{glyphs:?} contains an empty glyph"
            );
        }
    }

    #[test]
    fn cli_selects_playstation_symbols() {
        use clap::{Command, FromArgMatches as _, Subcommand as _};

        use crate::animation::AnimationKind;

        let command = AnimationKind::augment_subcommands(Command::new("anim"));
        let matches = command
            .try_get_matches_from(["anim", "matrix", "--glyphs", "playstation"])
            .unwrap();

        assert_eq!(
            AnimationKind::from_arg_matches(&matches).unwrap(),
            AnimationKind::Matrix(Options {
                glyphs: GlyphSet::Playstation,
                ..Options::default()
            })
        );
    }

    #[test]
    fn cli_selects_glyph_sets() {
        use clap::{Command, FromArgMatches as _, Subcommand as _};

        use crate::animation::AnimationKind;

        for (name, glyphs) in [
            ("katakana", GlyphSet::Katakana),
            ("binary", GlyphSet::Binary),
            ("playstation", GlyphSet::Playstation),
            ("maths", GlyphSet::Maths),
            ("ascii", GlyphSet::Ascii),
            ("minecraft", GlyphSet::Minecraft),
        ] {
            let command = AnimationKind::augment_subcommands(Command::new("anim"));
            let matches = command
                .try_get_matches_from(["anim", "matrix", "--glyphs", name])
                .unwrap();

            assert_eq!(
                AnimationKind::from_arg_matches(&matches).unwrap(),
                AnimationKind::Matrix(Options {
                    glyphs,
                    ..Options::default()
                })
            );
        }
    }
}

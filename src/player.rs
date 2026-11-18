use std::io;
use std::thread;
use std::time::{Duration, Instant};

use nusb::transfer::Buffer;

use crate::animation::{Animation, AnimationKind};
use crate::canvas::Canvas;
use crate::device::{Device, FRAME_BYTES, send_frame};

const IDLE_PERIOD: Duration = Duration::from_millis(20);

// Owns animation state, frame pacing, and the reusable wire buffer.
pub struct Player {
    kind: AnimationKind,
    anim: Box<dyn Animation>,
    canvas: Canvas,
    frame: Option<Buffer>,
    fps: u32,
    period: Duration,
    playing: bool,
    deadline: Instant,
}

impl Player {
    pub fn new(kind: AnimationKind, fps: u32) -> Self {
        assert!(fps > 0, "fps must be greater than zero");

        let anim = kind.create();
        Self {
            kind,
            anim,
            canvas: Canvas::new(),
            frame: Some(Buffer::from(vec![0u8; FRAME_BYTES])),
            fps,
            period: Duration::from_secs_f64(1.0 / fps as f64),
            playing: false,
            deadline: Instant::now(),
        }
    }

    pub fn kind(&self) -> &AnimationKind {
        &self.kind
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    // Select an animation and rate, then resume.
    pub fn play(&mut self, kind: AnimationKind, fps: u32) {
        assert!(fps > 0, "fps must be greater than zero");

        if kind != self.kind {
            self.anim = kind.create();
            self.kind = kind;
            self.canvas.clear();
        }
        if fps != self.fps {
            self.fps = fps;
            self.period = Duration::from_secs_f64(1.0 / fps as f64);
            if self.playing {
                self.deadline = Instant::now();
            }
        }

        self.resume();
    }

    pub fn resume(&mut self) {
        if !self.playing {
            self.playing = true;
            self.deadline = Instant::now();
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    // Render and send one frame, then sleep until the next one is due.
    pub fn step(&mut self, dev: &mut Device) -> io::Result<()> {
        if self.playing {
            self.anim.draw(&mut self.canvas, self.period);

            let mut frame = self.frame.take().expect("frame buffer missing");
            self.canvas.encode(&mut frame);
            self.frame = Some(send_frame(dev, frame)?);
        }

        // Transfer time must not lower the frame rate
        let period = if self.playing {
            self.period
        } else {
            IDLE_PERIOD
        };
        self.deadline += period;
        let now = Instant::now();
        if let Some(wait) = self.deadline.checked_duration_since(now) {
            thread::sleep(wait);
        } else {
            self.deadline = now;
        }

        Ok(())
    }
}

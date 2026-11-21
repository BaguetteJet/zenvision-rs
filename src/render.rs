//! Frame pacing. Rust owns the clock; animations only supply pixels.

use std::io;
use std::time::{Duration, Instant};

use crate::animation::{Animation, AnimationKind, Status};
use crate::canvas::Canvas;
use crate::panel::Panel;
use crate::protocol::Fps;

/// Cap the elapsed time handed to animations after a suspend or a long stall.
const MAX_DT: Duration = Duration::from_millis(250);

pub struct Player {
    kind: AnimationKind,
    anim: Box<dyn Animation>,
    canvas: Canvas,
    fps: Fps,
    period: Duration,
    playing: bool,
    next: Instant,
    last: Instant,
}

impl Player {
    pub fn new(kind: AnimationKind, fps: Fps) -> Self {
        let now = Instant::now();
        Self {
            anim: kind.create(),
            kind,
            canvas: Canvas::new(),
            fps,
            period: period(fps),
            playing: false,
            next: now,
            last: now,
        }
    }

    pub fn kind(&self) -> &AnimationKind {
        &self.kind
    }

    pub fn fps(&self) -> Fps {
        self.fps
    }

    /// When the next frame is due, or `None` while paused.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.playing.then_some(self.next)
    }

    /// Select an animation and rate, then resume.
    pub fn play(&mut self, kind: AnimationKind, fps: Fps) {
        if kind != self.kind {
            self.anim = kind.create();
            self.kind = kind;
            self.canvas.clear();
        }
        if fps != self.fps {
            self.fps = fps;
            self.period = period(fps);
        }
        self.resume();
    }

    pub fn resume(&mut self) {
        let now = Instant::now();
        self.playing = true;
        self.next = now;
        self.last = now;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Draw and display one frame, then schedule the next.
    ///
    /// Returns [`Status::Unchanged`] without touching the panel while paused.
    ///
    /// # Errors
    /// Propagates animation and panel failures.
    pub fn tick(&mut self, panel: &mut dyn Panel) -> io::Result<Status> {
        if !self.playing {
            return Ok(Status::Unchanged);
        }

        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last).min(MAX_DT);
        self.last = now;

        let status = self.anim.draw(&mut self.canvas, dt)?;
        match status {
            Status::New => panel.frame(&self.canvas)?,
            Status::Unchanged => {}
            Status::Ended => self.playing = false,
        }

        self.next = self.next.checked_add(self.period).unwrap_or(now);
        if self.next <= now {
            self.next = now + self.period;
        }

        Ok(status)
    }

    /// Forward optional live input to the current animation.
    ///
    /// # Errors
    /// Propagates animation failures.
    pub fn input(&mut self, input: &[u8]) -> io::Result<()> {
        self.anim.input(input)
    }
}

fn period(fps: Fps) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_player_has_no_deadline_and_never_wakes() {
        let player = Player::new(AnimationKind::default(), Fps::DEFAULT);
        assert!(player.next_deadline().is_none());
    }

    #[test]
    fn resuming_schedules_an_immediate_frame() {
        let mut player = Player::new(AnimationKind::default(), Fps::DEFAULT);
        player.resume();
        assert!(player.next_deadline().is_some());
        player.pause();
        assert!(player.next_deadline().is_none());
    }

    #[test]
    fn changing_fps_keeps_the_animation_instance() {
        let mut player = Player::new(AnimationKind::default(), Fps::DEFAULT);
        player.resume();
        player.play(AnimationKind::default(), Fps::new(60).unwrap());
        assert_eq!(player.fps(), Fps::new(60).unwrap());
        assert!(player.next_deadline().is_some());
    }
}

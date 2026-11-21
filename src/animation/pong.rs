//! Autonomous Pong.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Animation, Status};
use crate::canvas::{Canvas, HEIGHT, WIDTH};

const PADDLE_H: f32 = 12.0;
const PADDLE_W: i32 = 2;
const BALL_SIZE: i32 = 2;
const BALL_SPEED: f32 = 90.0;
const AI_SPEED: f32 = 55.0;
const WIN_SCORE: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, clap::Args)]
#[serde(default)]
pub struct Options {
    /// Points before the game resets
    #[arg(long, default_value_t = WIN_SCORE)]
    pub points: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { points: WIN_SCORE }
    }
}

pub struct Pong {
    ball_x: f32,
    ball_y: f32,
    vel_x: f32,
    vel_y: f32,
    left_y: f32,
    right_y: f32,
    left_score: u8,
    right_score: u8,
    points: u8,
}

impl Pong {
    pub fn new(options: Options) -> Self {
        Self {
            ball_x: WIDTH as f32 / 2.0,
            ball_y: HEIGHT as f32 / 2.0,
            vel_x: BALL_SPEED,
            vel_y: BALL_SPEED * 0.4,
            left_y: HEIGHT as f32 / 2.0,
            right_y: HEIGHT as f32 / 2.0,
            left_score: 0,
            right_score: 0,
            points: options.points.max(1),
        }
    }

    fn reset_ball(&mut self, direction: f32) {
        self.ball_x = WIDTH as f32 / 2.0;
        self.ball_y = HEIGHT as f32 / 2.0;
        self.vel_x = BALL_SPEED * direction;
        self.vel_y = BALL_SPEED * 0.4;
    }

    fn move_ai(paddle: &mut f32, target: f32, dt: f32) {
        let delta = target - *paddle;
        *paddle += delta.clamp(-AI_SPEED * dt, AI_SPEED * dt);
        *paddle = paddle.clamp(PADDLE_H / 2.0, HEIGHT as f32 - PADDLE_H / 2.0);
    }

    fn digit(canvas: &mut Canvas, x: i32, y: i32, value: u8) {
        const DIGITS: [[u8; 5]; 10] = [
            [7, 5, 5, 5, 7], // 0
            [2, 6, 2, 2, 7], // 1
            [7, 1, 7, 4, 7], // 2
            [7, 1, 7, 1, 7], // 3
            [5, 5, 7, 1, 1], // 4
            [7, 4, 7, 1, 7], // 5
            [7, 4, 7, 5, 7], // 6
            [7, 1, 1, 1, 1], // 7
            [7, 5, 7, 5, 7], // 8
            [7, 5, 7, 1, 7], // 9
        ];

        for (row, bits) in DIGITS[value.min(9) as usize].iter().enumerate() {
            for col in 0..3 {
                if bits & (1 << (2 - col)) != 0 {
                    canvas.plot(x + col, y + row as i32, 255);
                }
            }
        }
    }
}

impl Animation for Pong {
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration) -> io::Result<Status> {
        let dt = dt.as_secs_f32().min(0.1);

        // Basic agents follow the ball.
        Self::move_ai(&mut self.left_y, self.ball_y, dt);
        Self::move_ai(&mut self.right_y, self.ball_y, dt);

        self.ball_x += self.vel_x * dt;
        self.ball_y += self.vel_y * dt;

        // Top and bottom walls.
        if self.ball_y <= 1.0 || self.ball_y >= HEIGHT as f32 - 2.0 {
            self.vel_y = -self.vel_y;
            self.ball_y = self.ball_y.clamp(1.0, HEIGHT as f32 - 2.0);
        }

        // Left paddle.
        if self.vel_x < 0.0
            && self.ball_x <= 7.0
            && (self.ball_y - self.left_y).abs() <= PADDLE_H / 2.0
        {
            self.vel_x = -self.vel_x;
            self.vel_y += (self.ball_y - self.left_y) * 3.0;
            self.ball_x = 7.0;
        }

        // Right paddle.
        if self.vel_x > 0.0
            && self.ball_x >= WIDTH as f32 - 8.0
            && (self.ball_y - self.right_y).abs() <= PADDLE_H / 2.0
        {
            self.vel_x = -self.vel_x;
            self.vel_y += (self.ball_y - self.right_y) * 3.0;
            self.ball_x = WIDTH as f32 - 8.0;
        }

        // Scoring.
        if self.ball_x < 0.0 {
            self.right_score += 1;
            if self.right_score >= self.points {
                self.left_score = 0;
                self.right_score = 0;
            }
            self.reset_ball(-1.0);
        } else if self.ball_x >= WIDTH as f32 {
            self.left_score += 1;
            if self.left_score >= self.points {
                self.left_score = 0;
                self.right_score = 0;
            }
            self.reset_ball(1.0);
        }

        canvas.clear();

        // Center line.
        for y in (0..HEIGHT as i32).step_by(4) {
            canvas.plot(WIDTH as i32 / 2, y, 80);
        }

        // Paddles.
        for y in 0..PADDLE_H as i32 {
            for x in 0..PADDLE_W {
                canvas.plot(
                    4 + x,
                    self.left_y as i32 - PADDLE_H as i32 / 2 + y,
                    255,
                );
                canvas.plot(
                    WIDTH as i32 - 6 + x,
                    self.right_y as i32 - PADDLE_H as i32 / 2 + y,
                    255,
                );
            }
        }

        // Ball.
        for y in 0..BALL_SIZE {
            for x in 0..BALL_SIZE {
                canvas.plot(self.ball_x as i32 + x, self.ball_y as i32 + y, 255);
            }
        }

        // Score.
        Self::digit(canvas, WIDTH as i32 / 2 - 10, 2, self.left_score);
        Self::digit(canvas, WIDTH as i32 / 2 + 6, 2, self.right_score);

        Ok(Status::New)
    }
}
//! Panel abstraction: the real USB panel and a hardware-free terminal preview.

use std::io::{self, Write};

use nusb::DeviceId;

use crate::canvas::{Canvas, HEIGHT, WIDTH};
use crate::device::{Device, open_device};
use crate::protocol::{PanelCommand, STATE_QUERY, Time};

/// Anything that can receive panel commands and frames.
pub trait Panel {
    /// Apply one panel command. Only the state query returns text.
    ///
    /// # Errors
    /// Propagate transport failures.
    fn apply(&mut self, command: &PanelCommand) -> io::Result<Option<String>>;

    /// Display one frame.
    ///
    /// # Errors
    /// Propagate transport failures.
    fn frame(&mut self, canvas: &Canvas) -> io::Result<()>;

    /// USB identity, when this panel is backed by a device.
    fn device_id(&self) -> Option<DeviceId> {
        None
    }
}

/// The physical panel.
pub struct RealPanel {
    device: Device,
    frame: Option<nusb::transfer::Buffer>,
}

impl RealPanel {
    /// Open the physical panel.
    ///
    /// # Errors
    /// Fails when the device is absent or cannot be claimed.
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            device: open_device()?,
            frame: Some(Device::frame_buffer()),
        })
    }

    fn send_frame(&mut self, canvas: &Canvas) -> io::Result<()> {
        let mut frame = self.frame.take().unwrap_or_else(Device::frame_buffer);
        if let Err(error) = canvas.encode(&mut frame) {
            self.frame = Some(frame);
            return Err(error);
        }

        match self.device.send_frame(frame) {
            Ok(buffer) => {
                self.frame = Some(buffer);
                Ok(())
            }
            Err((error, buffer)) => {
                self.frame = Some(buffer);
                Err(error)
            }
        }
    }
}

impl Panel for RealPanel {
    fn apply(&mut self, command: &PanelCommand) -> io::Result<Option<String>> {
        if matches!(command, PanelCommand::State) {
            let reply = self.device.send_command(STATE_QUERY)?.unwrap_or_default();
            return Ok(Some(
                String::from_utf8_lossy(&reply)
                    .trim_end_matches('\0')
                    .to_string(),
            ));
        }

        for payload in command.payloads(Time::now()) {
            self.device.send_command(&payload)?;
        }
        Ok(None)
    }

    fn frame(&mut self, canvas: &Canvas) -> io::Result<()> {
        self.send_frame(canvas)
    }

    fn device_id(&self) -> Option<DeviceId> {
        Some(self.device.id())
    }
}

/// A tiny ANSI preview for developing animations without the panel.
pub struct TerminalPanel {
    cols: usize,
    rows: usize,
    first: bool,
}

impl TerminalPanel {
    pub fn new() -> Self {
        Self {
            cols: 64,
            rows: 16,
            first: true,
        }
    }
}

impl Default for TerminalPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for TerminalPanel {
    fn apply(&mut self, _command: &PanelCommand) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn frame(&mut self, canvas: &Canvas) -> io::Result<()> {
        const RAMP: &[u8] = b" .:-=+*#%@";
        let pixels = canvas.pixels();
        let block_x = WIDTH / self.cols;
        let block_y = HEIGHT / self.rows;

        let mut out = String::with_capacity(self.rows * (self.cols + 8) + 8);
        if self.first {
            out.push_str("\x1b[2J\x1b[?25l");
            self.first = false;
        }
        out.push_str("\x1b[H");

        for row in 0..self.rows {
            for col in 0..self.cols {
                let mut sum = 0u32;
                let mut count = 0u32;
                for y in row * block_y..(row + 1) * block_y {
                    let line = &pixels[y * WIDTH..(y + 1) * WIDTH];
                    for &pixel in &line[col * block_x..(col + 1) * block_x] {
                        sum += u32::from(pixel);
                        count += 1;
                    }
                }
                let level = (sum / count.max(1)) as usize * (RAMP.len() - 1) / 255;
                out.push(char::from(RAMP[level]));
            }
            out.push('\n');
        }

        let mut stdout = io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        let _ = writeln!(io::stdout(), "\x1b[?25h");
    }
}

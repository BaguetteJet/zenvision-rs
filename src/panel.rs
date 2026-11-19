//! Panel abstraction: the real USB panel and a hardware-free terminal preview.

use std::fmt::Write as _;
use std::io::{self, Write};

use nusb::DeviceId;

use crate::canvas::{Canvas, HEIGHT, WIDTH};
use crate::device::{Device, open_device};
use crate::preview::PreviewHub;
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

/// A real panel that also publishes accepted frames to the live preview.
pub struct TeePanel {
    inner: RealPanel,
    preview: PreviewHub,
}

impl TeePanel {
    /// Open the physical panel and attach the preview fan-out.
    ///
    /// # Errors
    /// Fails when the device is absent or cannot be claimed.
    pub fn open(preview: PreviewHub) -> io::Result<Self> {
        Ok(Self {
            inner: RealPanel::open()?,
            preview,
        })
    }
}

impl Panel for TeePanel {
    fn apply(&mut self, command: &PanelCommand) -> io::Result<Option<String>> {
        self.inner.apply(command)
    }

    fn frame(&mut self, canvas: &Canvas) -> io::Result<()> {
        self.inner.frame(canvas)?;
        self.preview.publish(canvas);
        Ok(())
    }

    fn device_id(&self) -> Option<DeviceId> {
        self.inner.device_id()
    }
}

/// One character covers a 2x1 pixel block: the upper half block paints the top
/// pixel in the foreground color and the bottom pixel in the background color.
const PREVIEW_ROWS: usize = HEIGHT / 2;

/// A hardware-free preview for developing animations.
///
/// Runs in the terminal's alternate screen, so leaving `preview` restores what
/// was on screen. A full-size preview is 256 columns by 32 rows; smaller
/// terminals get a nearest-neighbour resample that preserves the panel's 4:1
/// aspect ratio.
pub struct TerminalPanel {
    active: bool,
    out: String,
    last_frame: Vec<u8>,
    has_frame: bool,
    size: Option<(usize, usize)>,
}

impl TerminalPanel {
    pub fn new() -> Self {
        Self {
            active: false,
            out: String::with_capacity(WIDTH * PREVIEW_ROWS * 4),
            last_frame: Vec::with_capacity(WIDTH * HEIGHT),
            has_frame: false,
            size: None,
        }
    }

    /// Replace the preview with a centred status line.
    ///
    /// # Errors
    /// Propagates terminal write failures.
    pub fn notice(&mut self, text: &str) -> io::Result<()> {
        let size = terminal_size();
        self.out.clear();
        if !self.active {
            self.out.push_str("\x1b[?1049h\x1b[?25l");
            self.active = true;
        }
        self.out.push_str("\x1b[2J\x1b[H");

        let padding = size.0.saturating_sub(text.chars().count()) / 2;
        for _ in 0..padding {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.out.push('\n');

        self.size = Some(size);
        self.has_frame = false;

        let mut stdout = io::stdout().lock();
        stdout.write_all(self.out.as_bytes())?;
        stdout.flush()
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
        let size = terminal_size();
        let resized = self.size != Some(size);
        let pixels = canvas.pixels();

        // Static frames should not rewrite the terminal every tick.
        if !resized && self.has_frame && self.last_frame.as_slice() == pixels {
            return Ok(());
        }

        self.out.clear();
        if !self.active {
            // Alternate screen + hidden cursor; both are undone in `Drop`.
            self.out.push_str("\x1b[?1049h\x1b[?25l");
            self.active = true;
        }
        if resized {
            self.out.push_str("\x1b[2J");
            self.size = Some(size);
        }
        self.out.push_str("\x1b[H");
        render(canvas, &mut self.out, size.0, size.1);

        {
            let mut stdout = io::stdout().lock();
            stdout.write_all(self.out.as_bytes())?;
            stdout.flush()?;
        }

        if self.last_frame.len() != pixels.len() {
            self.last_frame.resize(pixels.len(), 0);
        }
        self.last_frame.copy_from_slice(pixels);
        self.has_frame = true;
        Ok(())
    }
}

impl Drop for TerminalPanel {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        // Reset colors, show the cursor and restore the original screen.
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = stdout.flush();
    }
}

fn terminal_size() -> (usize, usize) {
    // SAFETY: TIOCGWINSZ writes one `winsize` into the pointer, and
    // STDOUT_FILENO is always a valid descriptor for this call.
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    let result = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) };
    if result == 0 {
        // SAFETY: the ioctl succeeded, so it initialized the whole struct.
        let size = unsafe { size.assume_init() };
        if size.ws_col > 0 && size.ws_row > 0 {
            return (usize::from(size.ws_col), usize::from(size.ws_row));
        }
    }

    (WIDTH, PREVIEW_ROWS)
}

/// Paint the canvas with one `▀` per top/bottom pixel pair.
fn render(canvas: &Canvas, out: &mut String, cols: usize, rows: usize) {
    let pixels = canvas.pixels();

    // A character is about twice as tall as it is wide, so 256x64 pixels map
    // to 256x32 characters. Shrink both dimensions together to stay 4:1.
    let max_cols = cols.clamp(1, WIDTH);
    let max_rows = rows.clamp(1, PREVIEW_ROWS);
    let display_cols = max_cols.min(max_rows * 8);
    let display_rows = max_rows.min(display_cols / 8).max(1);
    let exact = display_cols == WIDTH && display_rows == PREVIEW_ROWS;

    for cy in 0..display_rows {
        let mut last_fg: Option<u8> = None;
        let mut last_bg: Option<u8> = None;

        for cx in 0..display_cols {
            let (top, bottom) = if exact {
                (
                    pixels[(cy * 2) * WIDTH + cx],
                    pixels[(cy * 2 + 1) * WIDTH + cx],
                )
            } else {
                let px = cx * WIDTH / display_cols;
                let top_y = (cy * 2) * HEIGHT / (display_rows * 2);
                let bottom_y = (cy * 2 + 1) * HEIGHT / (display_rows * 2);
                (
                    pixels[top_y.min(HEIGHT - 1) * WIDTH + px],
                    pixels[bottom_y.min(HEIGHT - 1) * WIDTH + px],
                )
            };

            if last_fg != Some(top) {
                let _ = write!(out, "\x1b[38;2;{top};{top};{top}m");
                last_fg = Some(top);
            }
            if last_bg != Some(bottom) {
                let _ = write!(out, "\x1b[48;2;{bottom};{bottom};{bottom}m");
                last_bg = Some(bottom);
            }
            out.push('▀');
        }

        // Reset colors, clear any remaining cells on this line, then advance.
        out.push_str("\x1b[0m\x1b[K\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_one_line_per_pixel_pair() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, WIDTH, PREVIEW_ROWS + 5);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), PREVIEW_ROWS);
        assert_eq!(lines[0].matches('▀').count(), WIDTH);
    }

    #[test]
    fn preview_maps_top_and_bottom_pixels_to_colors() {
        let mut canvas = Canvas::new();
        canvas.plot(0, 0, 255);
        canvas.plot(0, 1, 128);

        let mut text = String::new();
        render(&canvas, &mut text, WIDTH, PREVIEW_ROWS);

        assert!(text.starts_with("\x1b[38;2;255;255;255m\x1b[48;2;128;128;128m▀"));
    }

    #[test]
    fn preview_coalesces_repeated_colors() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, WIDTH, PREVIEW_ROWS);

        // A blank row needs one foreground and one background escape.
        assert!(text.matches("\x1b[38;2;").count() <= PREVIEW_ROWS);
        assert!(text.matches("\x1b[48;2;").count() <= PREVIEW_ROWS);
    }

    #[test]
    fn preview_fits_a_smaller_terminal_and_keeps_aspect() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, 40, 20);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].matches('▀').count(), 40);
    }
}

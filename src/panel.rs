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

/// One character covers a 2x3 pixel block.
const PREVIEW_COLS: usize = WIDTH.div_ceil(2);
const PREVIEW_ROWS: usize = HEIGHT.div_ceil(3);

/// A hardware-free preview for developing animations.
///
/// The preview runs in the terminal's alternate screen, so leaving `preview`
/// restores whatever was on screen before and leaves no characters behind.
/// Each character covers a 2x3 pixel block, so 256x64 becomes 128 columns by
/// 22 rows. Foreground and background are true grayscale ANSI colors; pixels
/// above the block mean become the foreground shape and the rest become the
/// background, which reproduces the mock display from `terminal-mock-display`.
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
            out: String::with_capacity(PREVIEW_COLS * PREVIEW_ROWS * 12),
            last_frame: Vec::with_capacity(WIDTH * HEIGHT),
            has_frame: false,
            size: None,
        }
    }

    /// Replace the preview with a centred status line.
    ///
    /// Used when the panel switches to a theme or clock and stops receiving
    /// host frames.
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

        // Static animations should not rewrite the terminal every tick.
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

    (PREVIEW_COLS, PREVIEW_ROWS)
}

/// Character for a 2x3 pixel block.
///
/// `pattern` has one bit per pixel: bit 0 top-left, bit 1 top-right, bit 2
/// middle-left, bit 3 middle-right, bit 4 bottom-left, bit 5 bottom-right.
/// The 60 sextant characters (U+1FB00..U+1FB3B) skip the patterns already
/// covered by `▌` (21), `▐` (42) and `█` (63).
fn sextant(pattern: u8) -> char {
    match pattern {
        0 => ' ',
        21 => '▌',
        42 => '▐',
        63 => '█',
        n => {
            let offset = u32::from(n) - 1 - u32::from(n > 21) - u32::from(n > 42);
            char::from_u32(0x1FB00 + offset).unwrap_or(' ')
        }
    }
}

fn render(canvas: &Canvas, out: &mut String, cols: usize, rows: usize) {
    let pixels = canvas.pixels();
    let display_cols = cols.clamp(1, PREVIEW_COLS);
    let display_rows = rows.clamp(1, PREVIEW_ROWS);

    // At full size each cell is the exact 2x3 pixel block from the mock
    // display. Smaller terminals get a nearest-neighbour resample instead of
    // wrapping or clipping the image.
    let exact = display_cols == PREVIEW_COLS && display_rows == PREVIEW_ROWS;

    for cy in 0..display_rows {
        let mut last_fg: Option<u8> = None;
        let mut last_bg: Option<u8> = None;
        let mut transparent = false;

        // The panel is 64 pixels high, so the last sextant cell has only its
        // top third on-screen. Do not paint a background for that cell.
        let partial_row = exact && cy + 1 == PREVIEW_ROWS && !HEIGHT.is_multiple_of(3);

        for cx in 0..display_cols {
            let mut block = [None; 6];
            let mut sum = 0u32;
            let mut count = 0u32;
            for dy in 0..3usize {
                for dx in 0..2usize {
                    let pixel = if exact {
                        let (px, py) = (cx * 2 + dx, cy * 3 + dy);
                        (px < WIDTH && py < HEIGHT).then(|| pixels[py * WIDTH + px])
                    } else {
                        let px = (cx * 2 + dx) * WIDTH / (display_cols * 2);
                        let py = (cy * 3 + dy) * HEIGHT / (display_rows * 3);
                        Some(pixels[py.min(HEIGHT - 1) * WIDTH + px.min(WIDTH - 1)])
                    };

                    if let Some(pixel) = pixel {
                        block[dy * 2 + dx] = Some(pixel);
                        sum += u32::from(pixel);
                        count += 1;
                    }
                }
            }
            let mean = sum / count;

            let mut pattern = 0u8;
            let mut fg_sum = 0u32;
            let mut fg_count = 0u32;
            let mut bg_sum = 0u32;
            let mut bg_count = 0u32;
            for (bit, pixel) in block.iter().enumerate() {
                let Some(pixel) = *pixel else { continue };
                let pixel = u32::from(pixel);
                if pixel >= mean {
                    pattern |= 1 << bit;
                    fg_sum += pixel;
                    fg_count += 1;
                } else {
                    bg_sum += pixel;
                    bg_count += 1;
                }
            }

            // Only emit color changes. A starfield row is mostly the same
            // black/transparent pair, which makes the output much smaller.
            let fg = (fg_sum / fg_count.max(1)) as u8;
            if last_fg != Some(fg) {
                let _ = write!(out, "\x1b[38;2;{fg};{fg};{fg}m");
                last_fg = Some(fg);
            }
            let background = if partial_row {
                None
            } else {
                bg_sum.checked_div(bg_count)
            };
            match background {
                Some(bg) => {
                    let bg = bg as u8;
                    if transparent || last_bg != Some(bg) {
                        let _ = write!(out, "\x1b[48;2;{bg};{bg};{bg}m");
                        last_bg = Some(bg);
                        transparent = false;
                    }
                }
                None => {
                    if !transparent {
                        out.push_str("\x1b[49m");
                        transparent = true;
                        last_bg = None;
                    }
                }
            }
            out.push(sextant(pattern));
        }

        // Reset colors, clear any remaining cells on this line, then advance.
        out.push_str("\x1b[0m\x1b[K\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sextant_maps_known_patterns() {
        assert_eq!(sextant(0), ' ');
        assert_eq!(sextant(1), '\u{1FB00}');
        assert_eq!(sextant(21), '▌');
        assert_eq!(sextant(22), '\u{1FB14}');
        assert_eq!(sextant(42), '▐');
        assert_eq!(sextant(62), '\u{1FB3B}');
        assert_eq!(sextant(63), '█');
    }

    #[test]
    fn preview_is_128_columns_by_22_rows() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, PREVIEW_COLS, PREVIEW_ROWS);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 22);
        assert_eq!(lines[0].matches('█').count(), 128);
    }

    #[test]
    fn preview_maps_pixels_to_grayscale() {
        let mut canvas = Canvas::new();
        canvas.plot(0, 0, 255);
        let mut text = String::new();
        render(&canvas, &mut text, PREVIEW_COLS, PREVIEW_ROWS);

        // The bright top-left pixel becomes the foreground of the first char.
        assert!(text.starts_with("\x1b[38;2;255;255;255m"));
    }

    #[test]
    fn last_partial_row_has_no_background() {
        let mut canvas = Canvas::new();
        for x in 0..WIDTH {
            canvas.plot(
                x as i32,
                (HEIGHT - 1) as i32,
                if x % 2 == 0 { 255 } else { 10 },
            );
        }

        let mut text = String::new();
        render(&canvas, &mut text, PREVIEW_COLS, PREVIEW_ROWS);

        // The last cell has only its top third on-screen. Its background must
        // stay transparent so it cannot paint the missing bottom two rows.
        let last = text.lines().last().unwrap();
        assert!(!last.contains("\x1b[48;2;"));
        assert!(last.contains("\x1b[49m"));
    }

    #[test]
    fn preview_coalesces_repeated_colors() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, PREVIEW_COLS, PREVIEW_ROWS);

        // One foreground and one transparent-background escape per row is
        // enough for a blank frame; there is no need for 128 of each.
        assert!(text.matches("\x1b[38;2;").count() <= PREVIEW_ROWS);
        assert!(text.matches("\x1b[49m").count() <= PREVIEW_ROWS);
    }

    #[test]
    fn preview_fits_a_smaller_terminal() {
        let canvas = Canvas::new();
        let mut text = String::new();
        render(&canvas, &mut text, 10, 2);

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].matches('█').count(), 10);
        assert_eq!(lines[1].matches('█').count(), 10);
    }
}

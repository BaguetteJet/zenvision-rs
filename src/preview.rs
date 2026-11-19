//! Live preview fan-out.
//!
//! The daemon publishes exactly the frames it sends to the panel. A preview
//! client connects to a dedicated socket and receives those frames, plus a
//! short status message when streaming starts or stops.
//!
//! Publication is skipped entirely when no preview client is connected, and the
//! frame channel holds at most one frame, so a slow preview can never slow down
//! the panel.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Duration;

use socket2::Socket;

use crate::canvas::{Canvas, GRAYSCALE_BYTES};
use crate::panel::{Panel, TerminalPanel};

const STATUS_STREAMING: &[u8] = b"streaming";
const STATUS_STOPPED: &[u8] = b"stopped";

const STATUS_INTERVAL: Duration = Duration::from_millis(50);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Publisher side, owned by the daemon and shared with the panel tee.
#[derive(Clone)]
pub struct PreviewHub {
    active: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
    frames: SyncSender<Vec<u8>>,
}

impl PreviewHub {
    /// Start the preview listener thread.
    pub fn spawn(listener: Socket) -> Self {
        let (frames, receiver) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicBool::new(false));
        let streaming = Arc::new(AtomicBool::new(false));

        let thread_active = Arc::clone(&active);
        let thread_streaming = Arc::clone(&streaming);
        if let Err(error) = thread::Builder::new()
            .name("zenvision-preview".into())
            .spawn(move || serve(listener, receiver, thread_active, thread_streaming))
        {
            eprintln!("warning: could not start preview thread: {error}");
        }

        Self {
            active,
            streaming,
            frames,
        }
    }

    /// Publish one frame that the panel actually accepted.
    pub fn publish(&self, canvas: &Canvas) {
        if !self.active.load(Ordering::Relaxed) || !self.streaming.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.frames.try_send(canvas.pixels().to_vec());
    }

    /// Tell preview clients whether the panel is receiving host frames.
    pub fn set_streaming(&self, streaming: bool) {
        self.streaming.store(streaming, Ordering::Relaxed);
    }
}

fn serve(
    listener: Socket,
    frames: Receiver<Vec<u8>>,
    active: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
) {
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                let Ok(mut stream) = socket_into_stream(socket) else {
                    continue;
                };
                let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                active.store(true, Ordering::Relaxed);
                let _ = serve_client(&mut stream, &frames, &streaming);
                active.store(false, Ordering::Relaxed);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("preview socket accept failed: {error}");
                return;
            }
        }
    }
}

fn serve_client(
    stream: &mut UnixStream,
    frames: &Receiver<Vec<u8>>,
    streaming: &AtomicBool,
) -> io::Result<()> {
    let mut last_streaming = streaming.load(Ordering::Relaxed);
    write_status(stream, last_streaming)?;

    // Drop anything queued before this client connected, but keep the newest
    // frame so a streaming preview starts with a picture immediately.
    let mut latest = None;
    while let Ok(frame) = frames.try_recv() {
        latest = Some(frame);
    }
    if last_streaming && let Some(frame) = latest {
        stream.write_all(&frame)?;
    }

    loop {
        match frames.recv_timeout(STATUS_INTERVAL) {
            Ok(frame) => {
                let now = streaming.load(Ordering::Relaxed);
                if now != last_streaming {
                    write_status(stream, now)?;
                    last_streaming = now;
                }
                if now {
                    stream.write_all(&frame)?;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let now = streaming.load(Ordering::Relaxed);
                // Repeating the current status also detects a client that
                // disconnected while the panel was stopped.
                write_status(stream, now)?;
                last_streaming = now;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn write_status(stream: &mut UnixStream, streaming: bool) -> io::Result<()> {
    stream.write_all(if streaming {
        STATUS_STREAMING
    } else {
        STATUS_STOPPED
    })
}

fn socket_into_stream(socket: Socket) -> io::Result<UnixStream> {
    let owned = socket.as_fd().try_clone_to_owned()?;
    Ok(UnixStream::from(owned))
}

/// Client side: mirror daemon frames in the terminal until EOF or Ctrl-C.
///
/// # Errors
/// Propagates terminal writes and canvas conversion failures.
pub fn follow(mut stream: UnixStream, terminal: &mut TerminalPanel) -> io::Result<()> {
    let stop = crate::app::stop_flag()?;
    stream.set_read_timeout(Some(STATUS_INTERVAL))?;

    let mut canvas = Canvas::new();
    let mut frame = vec![0u8; GRAYSCALE_BYTES];
    let mut last_streaming = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        match stream.read(&mut frame) {
            Ok(GRAYSCALE_BYTES) => {
                canvas.set_grayscale(&frame)?;
                terminal.frame(&canvas)?;
            }
            Ok(0) => return Ok(()),
            Ok(size) => {
                let status = &frame[..size];
                let streaming = if status == STATUS_STREAMING {
                    true
                } else if status == STATUS_STOPPED {
                    false
                } else {
                    continue;
                };

                if last_streaming != Some(streaming) {
                    if !streaming {
                        terminal
                            .notice("panel is not streaming (built-in display or disconnected)")?;
                    }
                    last_streaming = Some(streaming);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return Ok(()),
        }
    }
}

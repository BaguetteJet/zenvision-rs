//! Live preview fan-out.
//!
//! The daemon keeps the newest frame the panel actually accepted. Preview
//! clients poll that shared slot, so any number of them can watch, a slow
//! client only skips frames, and no client can slow down the panel.
//!
//! Each seqpacket message is one byte of tag plus its payload: `0` is a raw
//! frame, `1` means streaming started, `2` means it stopped.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use socket2::Socket;

use crate::canvas::{Canvas, GRAYSCALE_BYTES};
use crate::panel::{Panel, TerminalPanel};

const TAG_FRAME: u8 = 0;
const TAG_STREAMING: u8 = 1;
const TAG_STOPPED: u8 = 2;

const POLL: Duration = Duration::from_millis(16);
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

struct Shared {
    latest: Mutex<Option<(u64, Vec<u8>)>>,
    seq: AtomicU64,
    streaming: AtomicBool,
    clients: AtomicUsize,
}

/// Publisher side, owned by the daemon and shared with the panel tee.
#[derive(Clone)]
pub struct PreviewHub(Arc<Shared>);

impl PreviewHub {
    /// Start the preview listener thread.
    pub fn spawn(listener: Socket) -> Self {
        let hub = Self(Arc::new(Shared {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            streaming: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
        }));

        let shared = Arc::clone(&hub.0);
        if let Err(error) = thread::Builder::new()
            .name("zenvision-preview".into())
            .spawn(move || serve(listener, shared))
        {
            eprintln!("warning: could not start preview thread: {error}");
        }

        hub
    }

    /// Publish one frame that the panel accepted. Does nothing without viewers.
    pub fn publish(&self, canvas: &Canvas) {
        if self.0.clients.load(Ordering::Relaxed) == 0 || !self.0.streaming.load(Ordering::Relaxed)
        {
            return;
        }

        let seq = self.0.seq.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut latest) = self.0.latest.lock() {
            *latest = Some((seq, canvas.pixels().to_vec()));
        }
    }

    /// Tell preview clients whether the panel is receiving host frames.
    pub fn set_streaming(&self, streaming: bool) {
        self.0.streaming.store(streaming, Ordering::Relaxed);
    }
}

fn serve(listener: Socket, shared: Arc<Shared>) {
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                let Ok(stream) = socket_into_stream(socket) else {
                    continue;
                };
                let shared = Arc::clone(&shared);
                let _ = thread::Builder::new()
                    .name("zenvision-preview-client".into())
                    .spawn(move || {
                        shared.clients.fetch_add(1, Ordering::Relaxed);
                        let _ = serve_client(stream, &shared);
                        shared.clients.fetch_sub(1, Ordering::Relaxed);
                    });
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("preview socket accept failed: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve_client(mut stream: UnixStream, shared: &Shared) -> io::Result<()> {
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

    let mut last_seq = 0u64;
    let mut last_streaming = None;
    let mut last_status: Option<Instant> = None;
    let mut packet = Vec::with_capacity(GRAYSCALE_BYTES + 1);

    loop {
        let streaming = shared.streaming.load(Ordering::Relaxed);
        let now = Instant::now();
        let status_due = last_status.is_none_or(|sent| now.duration_since(sent) >= STATUS_INTERVAL);

        // Repeating the status once a second detects a client that went away
        // while the panel was stopped.
        if last_streaming != Some(streaming) || status_due {
            stream.write_all(&[if streaming {
                TAG_STREAMING
            } else {
                TAG_STOPPED
            }])?;
            last_streaming = Some(streaming);
            last_status = Some(now);
        }

        let frame = if streaming {
            let latest = shared
                .latest
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match latest.as_ref() {
                Some((seq, frame)) if *seq != last_seq => {
                    last_seq = *seq;
                    Some(frame.clone())
                }
                _ => None,
            }
        } else {
            // Do not replay an old frame when streaming resumes.
            if let Ok(latest) = shared.latest.lock() {
                if let Some((seq, _)) = latest.as_ref() {
                    last_seq = *seq;
                }
            }
            None
        };

        if let Some(frame) = frame {
            packet.clear();
            packet.push(TAG_FRAME);
            packet.extend_from_slice(&frame);
            stream.write_all(&packet)?;
        }

        thread::sleep(POLL);
    }
}

fn socket_into_stream(socket: Socket) -> io::Result<UnixStream> {
    let owned = socket.as_fd().try_clone_to_owned()?;
    Ok(UnixStream::from(owned))
}

/// Client side: write raw frames to stdout for an external viewer.
///
/// # Errors
/// Propagates stdin/stdout failures.
pub fn follow_raw(mut stream: UnixStream) -> io::Result<()> {
    let stop = crate::app::stop_flag()?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    let mut frame = vec![0u8; GRAYSCALE_BYTES + 1];
    let mut stdout = io::stdout().lock();

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        match stream.read(&mut frame) {
            Ok(0) => return Ok(()),
            Ok(size) if frame[0] == TAG_FRAME && size == GRAYSCALE_BYTES + 1 => {
                stdout.write_all(&frame[1..])?;
                stdout.flush()?;
            }
            Ok(_) => {}
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

/// Client side: mirror daemon frames in the terminal until EOF or Ctrl-C.
///
/// # Errors
/// Propagates terminal writes and canvas conversion failures.
pub fn follow(mut stream: UnixStream, terminal: &mut TerminalPanel) -> io::Result<()> {
    let stop = crate::app::stop_flag()?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    let mut canvas = Canvas::new();
    let mut frame = vec![0u8; GRAYSCALE_BYTES + 1];
    let mut streaming = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        match stream.read(&mut frame) {
            Ok(0) => return Ok(()),
            Ok(size) => match frame[0] {
                TAG_FRAME if size == GRAYSCALE_BYTES + 1 => {
                    canvas.set_grayscale(&frame[1..])?;
                    terminal.frame(&canvas)?;
                }
                TAG_STREAMING => streaming = Some(true),
                TAG_STOPPED if streaming != Some(false) => {
                    terminal.notice("panel is not streaming (built-in display or disconnected)")?;
                    streaming = Some(false);
                }
                _ => {}
            },
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

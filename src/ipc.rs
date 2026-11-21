//! Unix-socket IPC and the external frame transport.
//!
//! The control socket speaks one versioned JSON request and one JSON reply per
//! connection. The frame socket is `SOCK_SEQPACKET`: each message starts with a
//! tag byte, `0` for a raw frame and `1` for animation input. External producers
//! never execute commands; they only supply pixels, and disconnecting releases
//! the display.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use socket2::{Domain, SockAddr, Socket, Type};

use crate::animation::AnimationKind;
use crate::canvas::GRAYSCALE_BYTES;
use crate::event::Event;
use crate::protocol::{Fps, PanelCommand};

/// Bumped whenever the JSON schema changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_REPLY_BYTES: u64 = 64 * 1024;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A producer that is silent for this long is disconnected so the built-in
/// animation can take over and another producer can connect.
const PRODUCER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// First byte of a seqpacket frame-socket message.
pub const TAG_FRAME: u8 = 0;
/// First byte of a seqpacket animation-input message.
pub const TAG_INPUT: u8 = 1;

const FRAME_MESSAGE_BYTES: usize = GRAYSCALE_BYTES + 1;
/// One byte of slack lets `read` distinguish an oversized datagram from a
/// valid one; SOCK_SEQPACKET truncates anything larger.
const RECEIVE_BYTES: usize = FRAME_MESSAGE_BYTES + 1;
/// Enough buffers for the frame thread, the pending slot and the display.
const RECYCLED_BUFFERS: usize = 4;

/// The newest frame waiting for the event loop.
///
/// The frame thread drains its socket continuously and replaces this slot, so
/// overload drops old frames instead of building latency. Buffers are returned
/// to a small pool for reuse.
#[derive(Default)]
pub struct FrameMailbox {
    newest: Mutex<Option<Vec<u8>>>,
    recycled: Mutex<Vec<Vec<u8>>>,
}

impl FrameMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `frame`, replacing and recycling any older pending frame.
    ///
    /// Returns true when the slot was empty, meaning the event loop needs a
    /// `FrameReady` notification.
    pub(crate) fn put(&self, frame: Vec<u8>) -> bool {
        let (was_empty, old) = {
            let mut newest = self
                .newest
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let was_empty = newest.is_none();
            (was_empty, newest.replace(frame))
        };

        if let Some(old) = old {
            self.recycle(old);
        }
        was_empty
    }

    pub(crate) fn take(&self) -> Option<Vec<u8>> {
        self.newest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.newest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    pub(crate) fn clear(&self) {
        if let Some(frame) = self.take() {
            self.recycle(frame);
        }
    }

    pub(crate) fn recycle(&self, frame: Vec<u8>) {
        let mut recycled = self
            .recycled
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if recycled.len() < RECYCLED_BUFFERS {
            recycled.push(frame);
        }
    }

    fn acquire(&self) -> Vec<u8> {
        self.recycled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop()
            .unwrap_or_else(|| vec![0u8; RECEIVE_BYTES])
    }
}

/// One control-socket request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub command: Command,
}

/// App-level commands. Panel commands are forwarded to [`PanelCommand`];
/// `play` additionally selects a native animation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Panel(PanelCommand),
    Play {
        animation: Option<AnimationKind>,
        fps: Option<Fps>,
    },
}

/// One control-socket reply.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Reply {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        message: String,
    },
}

impl Reply {
    pub const fn ok() -> Self {
        Self::Ok { message: None }
    }

    pub fn message(message: impl Into<String>) -> Self {
        Self::Ok {
            message: Some(message.into()),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

/// A request waiting for the event loop to produce a reply.
pub struct Incoming {
    pub command: Command,
    pub reply: Sender<Reply>,
}

/// Socket locations for this user.
#[derive(Clone, Debug)]
pub struct Paths {
    pub control: PathBuf,
    pub frames: PathBuf,
    pub preview: PathBuf,
}

impl Paths {
    /// Build paths inside the user runtime directory, never bare `/tmp`.
    ///
    /// # Errors
    /// Fails when the private fallback directory cannot be created.
    pub fn new() -> io::Result<Self> {
        let directory = runtime_dir()?;
        Ok(Self {
            control: directory.join("zenvision.sock"),
            frames: directory.join("zenvision-frames.sock"),
            preview: directory.join("zenvision-preview.sock"),
        })
    }
}

fn runtime_dir() -> io::Result<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        // The user's runtime directory is already private and owned by them;
        // never create or chmod it.
        return Ok(PathBuf::from(directory));
    }

    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let directory = std::env::temp_dir().join(format!("zenvision-{uid}"));
    private_dir(&directory, uid)?;
    Ok(directory)
}

/// Create the fallback directory and make sure it is ours and private.
fn private_dir(path: &Path, uid: libc::uid_t) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", path.display()),
        ));
    }
    if metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not owned by uid {uid}", path.display()),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Whether a daemon is already accepting control connections.
pub fn is_running(paths: &Paths) -> bool {
    UnixStream::connect(&paths.control).is_ok()
}

/// Bind the control, frame and preview sockets.
///
/// The preview listener is returned separately so the caller can hand it to
/// [`crate::preview::PreviewHub`].
///
/// # Errors
/// Fails when a socket path cannot be bound or listened on.
pub fn bind(paths: &Paths) -> io::Result<(Server, Socket)> {
    let _ = fs::remove_file(&paths.control);
    let control = UnixListener::bind(&paths.control)?;
    fs::set_permissions(&paths.control, fs::Permissions::from_mode(0o600))?;
    control.set_nonblocking(false)?;

    let frames = bind_seqpacket(&paths.frames)?;
    let preview = bind_seqpacket(&paths.preview)?;

    Ok((Server { control, frames }, preview))
}

fn bind_seqpacket(path: &Path) -> io::Result<Socket> {
    let _ = fs::remove_file(path);
    let socket = Socket::new(Domain::UNIX, Type::SEQPACKET, None)?;
    socket.bind(&SockAddr::unix(path)?)?;
    socket.listen(4)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(socket)
}

pub struct Server {
    control: UnixListener,
    frames: Socket,
}

impl Server {
    /// Accept control connections on their own lightweight threads, and serve
    /// one frame producer at a time.
    ///
    /// A stalled client only ties up its own thread: reads have a timeout and a
    /// length cap, so the frame loop is never blocked.
    pub fn spawn(self, events: Sender<Event>, frames: Arc<FrameMailbox>) {
        let control_events = events.clone();
        if let Err(error) = thread::Builder::new()
            .name("zenvision-ipc".into())
            .spawn(move || serve_control(self.control, control_events))
        {
            eprintln!("warning: could not start IPC thread: {error}");
        }

        if let Err(error) = thread::Builder::new()
            .name("zenvision-frames".into())
            .spawn(move || serve_frames(self.frames, events, frames))
        {
            eprintln!("warning: could not start frame thread: {error}");
        }
    }
}

fn serve_control(listener: UnixListener, events: Sender<Event>) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let events = events.clone();
                let _ = thread::Builder::new()
                    .name("zenvision-client".into())
                    .spawn(move || serve_client(stream, events));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("control socket accept failed: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn serve_client(stream: UnixStream, events: Sender<Event>) {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));

    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(message) => {
            let _ = write_reply(&stream, &Reply::error(message));
            return;
        }
    };

    let (reply_tx, reply_rx) = mpsc::channel();
    if events
        .send(Event::Command(Incoming {
            command: request.command,
            reply: reply_tx,
        }))
        .is_err()
    {
        return;
    }

    let reply = reply_rx
        .recv_timeout(CLIENT_TIMEOUT)
        .unwrap_or_else(|_| Reply::error("daemon did not reply in time"));
    let _ = write_reply(&stream, &reply);
}

/// The only field needed to decide whether the sender can be understood.
#[derive(Deserialize)]
struct Envelope {
    version: u32,
}

fn read_request(stream: &UnixStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader
        .by_ref()
        .take(MAX_REQUEST_BYTES + 1)
        .read_line(&mut line)
        .map_err(|error| format!("could not read request: {error}"))?;

    if read == 0 {
        return Err("empty request".into());
    }
    if !line.ends_with('\n') {
        return Err("request is too large or incomplete".into());
    }

    // Check the envelope before the command: a newer client with a new
    // command should hear about the version mismatch, not an unknown variant.
    let envelope: Envelope =
        serde_json::from_str(&line).map_err(|error| format!("invalid request: {error}"))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(format!(
            "protocol version {} does not match daemon version {}; restart the daemon",
            envelope.version, PROTOCOL_VERSION
        ));
    }

    serde_json::from_str(&line).map_err(|error| format!("invalid request: {error}"))
}

fn write_reply(stream: &UnixStream, reply: &Reply) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(reply).map_err(io::Error::other)?;
    encoded.push(b'\n');
    let mut writer = stream;
    writer.write_all(&encoded)?;
    writer.flush()
}

fn serve_frames(listener: Socket, events: Sender<Event>, frames: Arc<FrameMailbox>) {
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                let Ok(stream) = socket_into_stream(socket) else {
                    continue;
                };
                read_frames(&stream, &events, &frames);
                let _ = events.send(Event::ProducerGone);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("frame socket accept failed: {error}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn read_frames(mut stream: &UnixStream, events: &Sender<Event>, frames: &FrameMailbox) {
    if stream
        .set_read_timeout(Some(PRODUCER_IDLE_TIMEOUT))
        .is_err()
    {
        return;
    }

    let mut buffer = frames.acquire();
    loop {
        buffer.resize(RECEIVE_BYTES, 0);
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(size) if size > FRAME_MESSAGE_BYTES => {
                // A truncated oversized datagram; drop it rather than treat it
                // as a valid frame.
            }
            Ok(size) if size > 0 => {
                if buffer[0] == TAG_FRAME && size == FRAME_MESSAGE_BYTES {
                    buffer.copy_within(1..size, 0);
                    buffer.truncate(GRAYSCALE_BYTES);

                    // Keep draining the socket even if the loop has not taken
                    // the previous frame. Only the newest one survives.
                    let next = frames.acquire();
                    let frame = std::mem::replace(&mut buffer, next);
                    if frames.put(frame) && events.send(Event::FrameReady).is_err() {
                        return;
                    }
                } else if buffer[0] == TAG_INPUT
                    && size > 1
                    && events.send(Event::Input(buffer[1..size].to_vec())).is_err()
                {
                    return;
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                // Idle producer: release the display and let the next client in.
                return;
            }
            Err(_) => return,
        }
    }
}

fn socket_into_stream(socket: Socket) -> io::Result<UnixStream> {
    let owned = socket.as_fd().try_clone_to_owned()?;
    Ok(UnixStream::from(owned))
}

/// Send one request to a running daemon.
///
/// Returns `Ok(None)` when no daemon is listening.
///
/// # Errors
/// Propagates connection and serialization errors.
pub fn send(paths: &Paths, command: Command) -> io::Result<Option<Reply>> {
    let mut stream = match UnixStream::connect(&paths.control) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;

    let request = Request {
        version: PROTOCOL_VERSION,
        command,
    };
    let mut encoded = serde_json::to_vec(&request).map_err(io::Error::other)?;
    encoded.push(b'\n');
    stream.write_all(&encoded)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.by_ref().take(MAX_REPLY_BYTES).read_line(&mut line)?;
    if line.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "daemon closed the connection without replying",
        ));
    }

    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}

/// Connect to the daemon's frame socket.
///
/// # Errors
/// Fails when the daemon is absent or the socket cannot be created.
pub fn connect_frames(paths: &Paths) -> io::Result<UnixStream> {
    let socket = Socket::new(Domain::UNIX, Type::SEQPACKET, None)?;
    socket.connect(&SockAddr::unix(&paths.frames)?)?;
    socket_into_stream(socket)
}

/// Connect to the daemon's preview socket.
///
/// Returns `Ok(None)` when no daemon is listening.
///
/// # Errors
/// Propagates socket creation and connection errors other than "not running".
pub fn connect_preview(paths: &Paths) -> io::Result<Option<UnixStream>> {
    let socket = Socket::new(Domain::UNIX, Type::SEQPACKET, None)?;
    match socket.connect(&SockAddr::unix(&paths.preview)?) {
        Ok(()) => Ok(Some(socket_into_stream(socket)?)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PanelCommand;

    #[test]
    fn request_round_trips_as_json() {
        let request = Request {
            version: PROTOCOL_VERSION,
            command: Command::Panel(PanelCommand::Theme {
                value: crate::protocol::Theme::new(4).unwrap(),
            }),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(
            decoded.command,
            Command::Panel(PanelCommand::Theme { .. })
        ));
    }

    #[test]
    fn reply_round_trips_as_json() {
        for reply in [Reply::ok(), Reply::message("hello"), Reply::error("nope")] {
            let encoded = serde_json::to_string(&reply).unwrap();
            let decoded: Reply = serde_json::from_str(&encoded).unwrap();
            assert_eq!(encoded, serde_json::to_string(&decoded).unwrap());
        }
    }

    #[test]
    fn version_is_checked_before_the_command_shape() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .write_all(b"{\"version\":2,\"command\":{\"hologram\":{}}}\n")
            .unwrap();

        let error = read_request(&server).unwrap_err();
        assert!(error.contains("protocol version 2"), "{error}");
        assert!(!error.contains("hologram"), "{error}");
    }

    #[test]
    fn frame_messages_are_not_confused_with_input() {
        let (server, mut client) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let stream = socket_into_stream(server).unwrap();

        let (events_tx, events_rx) = mpsc::channel();
        let frames = Arc::new(FrameMailbox::new());
        let reader_frames = Arc::clone(&frames);
        let reader = thread::spawn(move || read_frames(&stream, &events_tx, &reader_frames));

        let frame = vec![7u8; GRAYSCALE_BYTES];
        let mut message = vec![TAG_FRAME];
        message.extend_from_slice(&frame);
        client.write_all(&message).unwrap();
        assert!(matches!(events_rx.recv().unwrap(), Event::FrameReady));
        let received = frames.take().unwrap();
        assert_eq!(received.len(), GRAYSCALE_BYTES);
        frames.recycle(received);

        client.write_all(&[TAG_INPUT, b'h', b'i']).unwrap();
        match events_rx.recv().unwrap() {
            Event::Input(input) => assert_eq!(input, b"hi"),
            _ => panic!("input tag should be input"),
        }

        drop(client);
        reader.join().unwrap();
    }

    #[test]
    fn newest_frame_replaces_the_pending_one() {
        let frames = FrameMailbox::new();
        assert!(frames.put(vec![1u8; GRAYSCALE_BYTES]));
        assert!(!frames.put(vec![2u8; GRAYSCALE_BYTES]));
        assert!(frames.has_frame());

        let newest = frames.take().unwrap();
        assert_eq!(newest[0], 2);
        assert!(!frames.has_frame());
    }

    #[test]
    fn oversized_frames_are_dropped() {
        let (server, mut client) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let stream = socket_into_stream(server).unwrap();

        let (events_tx, events_rx) = mpsc::channel();
        let frames = Arc::new(FrameMailbox::new());
        let reader = thread::spawn(move || read_frames(&stream, &events_tx, &frames));

        client
            .write_all(&vec![TAG_FRAME; FRAME_MESSAGE_BYTES + 1])
            .unwrap();
        assert!(events_rx.recv_timeout(Duration::from_millis(50)).is_err());

        drop(client);
        reader.join().unwrap();
    }

    #[test]
    fn fallback_directory_is_private() {
        use std::sync::atomic::{AtomicU32, Ordering};

        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let uid = unsafe { libc::geteuid() };
        let path = std::env::temp_dir().join(format!(
            "zenvision-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);

        private_dir(&path, uid).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::remove_dir_all(&path).unwrap();
    }
}

//! Unix-socket IPC and the external frame transport.
//!
//! The control socket speaks one versioned JSON request and one JSON reply per
//! connection. The frame socket is `SOCK_SEQPACKET`: each message is exactly
//! one raw grayscale frame. External producers never execute commands; they
//! only supply pixels, and disconnecting releases the display.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
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
        })
    }
}

fn runtime_dir() -> io::Result<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_RUNTIME_DIR") {
        let directory = PathBuf::from(directory);
        private_dir(&directory)?;
        return Ok(directory);
    }

    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let directory = std::env::temp_dir().join(format!("zenvision-{uid}"));
    private_dir(&directory)?;
    Ok(directory)
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Whether a daemon is already accepting control connections.
pub fn is_running(paths: &Paths) -> bool {
    UnixStream::connect(&paths.control).is_ok()
}

/// Bind both sockets.
///
/// # Errors
/// Fails when a socket path cannot be bound or listened on.
pub fn bind(paths: &Paths) -> io::Result<Server> {
    let _ = fs::remove_file(&paths.control);
    let control = UnixListener::bind(&paths.control)?;
    fs::set_permissions(&paths.control, fs::Permissions::from_mode(0o600))?;
    control.set_nonblocking(false)?;

    let _ = fs::remove_file(&paths.frames);
    let frames = Socket::new(Domain::UNIX, Type::SEQPACKET, None)?;
    frames.bind(&SockAddr::unix(&paths.frames)?)?;
    frames.listen(4)?;
    fs::set_permissions(&paths.frames, fs::Permissions::from_mode(0o600))?;

    Ok(Server { control, frames })
}

pub struct Server {
    control: UnixListener,
    frames: Socket,
}

impl Server {
    /// Accept control connections on their own lightweight threads, and serve
    /// at most one frame producer at a time.
    ///
    /// A stalled client only ties up its own thread: reads have a timeout and a
    /// length cap, so the frame loop is never blocked.
    pub fn spawn(
        self,
        events: Sender<Event>,
        frame_tx: SyncSender<Vec<u8>>,
        spare_rx: Receiver<Vec<u8>>,
    ) {
        let control_events = events.clone();
        if let Err(error) = thread::Builder::new()
            .name("zenvision-ipc".into())
            .spawn(move || serve_control(self.control, control_events))
        {
            eprintln!("warning: could not start IPC thread: {error}");
        }

        if let Err(error) = thread::Builder::new()
            .name("zenvision-frames".into())
            .spawn(move || serve_frames(self.frames, events, frame_tx, spare_rx))
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
                return;
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

    if request.version != PROTOCOL_VERSION {
        let _ = write_reply(
            &stream,
            &Reply::error(format!(
                "protocol version {} does not match daemon version {}; restart the daemon",
                request.version, PROTOCOL_VERSION
            )),
        );
        return;
    }

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

    serde_json::from_str(&line).map_err(|error| format!("invalid request: {error}"))
}

fn write_reply(stream: &UnixStream, reply: &Reply) -> io::Result<()> {
    let mut encoded = serde_json::to_vec(reply).map_err(io::Error::other)?;
    encoded.push(b'\n');
    let mut writer = stream;
    writer.write_all(&encoded)?;
    writer.flush()
}

fn serve_frames(
    listener: Socket,
    events: Sender<Event>,
    frame_tx: SyncSender<Vec<u8>>,
    spare_rx: Receiver<Vec<u8>>,
) {
    loop {
        match listener.accept() {
            Ok((socket, _)) => {
                let Ok(stream) = socket_into_stream(socket) else {
                    continue;
                };
                read_frames(&stream, &events, &frame_tx, &spare_rx);
                let _ = events.send(Event::ProducerGone);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("frame socket accept failed: {error}");
                return;
            }
        }
    }
}

fn read_frames(
    mut stream: &UnixStream,
    events: &Sender<Event>,
    frame_tx: &SyncSender<Vec<u8>>,
    spare_rx: &Receiver<Vec<u8>>,
) {
    let mut buffer = vec![0u8; GRAYSCALE_BYTES];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(GRAYSCALE_BYTES) => {
                if frame_tx.send(buffer).is_err() || events.send(Event::FrameReady).is_err() {
                    return;
                }
                // Wait for the loop to hand the buffer back. This bounds the
                // pipeline to one frame and lets the producer self-pace.
                buffer = spare_rx
                    .recv()
                    .unwrap_or_else(|_| vec![0u8; GRAYSCALE_BYTES]);
            }
            Ok(size) => {
                // Short messages are animation input, not frames.
                if events.send(Event::Input(buffer[..size].to_vec())).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PanelCommand, Theme};

    #[test]
    fn request_round_trips_as_json() {
        let request = Request {
            version: PROTOCOL_VERSION,
            command: Command::Panel(PanelCommand::Theme { value: Theme::Four }),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: Request = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(
            decoded.command,
            Command::Panel(PanelCommand::Theme { value: Theme::Four })
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
    fn frame_messages_are_not_confused_with_input() {
        let (server, mut client) = Socket::pair(Domain::UNIX, Type::SEQPACKET, None).unwrap();
        let stream = socket_into_stream(server).unwrap();

        let (events_tx, events_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
        let (spare_tx, spare_rx) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || read_frames(&stream, &events_tx, &frame_tx, &spare_rx));

        let frame = vec![7u8; GRAYSCALE_BYTES];
        client.write_all(&frame).unwrap();
        assert!(matches!(events_rx.recv().unwrap(), Event::FrameReady));
        let received = frame_rx.recv().unwrap();
        assert_eq!(received.len(), GRAYSCALE_BYTES);
        spare_tx.send(received).unwrap();

        client.write_all(b"hi").unwrap();
        match events_rx.recv().unwrap() {
            Event::Input(input) => assert_eq!(input, b"hi"),
            _ => panic!("short message should be input"),
        }

        drop(client);
        reader.join().unwrap();
    }
}

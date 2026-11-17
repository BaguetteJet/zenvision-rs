use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use clap::Parser;

use crate::usb::{Dev, open_device};
use crate::{Cli, Command, anim, commands, execute};

// Fixed frame rate for the background stream.
const FPS: u32 = 20;

// Per-user socket: XDG_RUNTIME_DIR when available, otherwise the temp dir.
fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("zenvision.sock")
}

// Forward one CLI invocation to a running daemon. None if none is running.
pub fn send(args: &[String]) -> io::Result<Option<String>> {
    let mut stream = match UnixStream::connect(socket_path()) {
        Ok(stream) => stream,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };

    writeln!(stream, "{}", args.join(" "))?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(Some(reply.trim_end().to_string()))
}

pub fn run() -> io::Result<()> {
    let path = socket_path();

    if UnixStream::connect(&path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("daemon is already running on {}", path.display()),
        ));
    }

    let mut dev = open_device()?;

    // Set the panel clock once on start; the built-in clock runs on its own after.
    if let Err(e) = commands::set_time(&mut dev, true) {
        eprintln!("warning: could not set panel time: {e}");
    }

    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    println!("zenvision daemon on {}", path.display());

    let mut daemon = Daemon {
        listener,
        streaming: false,
    };
    anim::run_with(&mut dev, FPS, |dev| daemon.tick(dev))
}

struct Daemon {
    listener: UnixListener,
    streaming: bool,
}

impl Daemon {
    // Serve pending clients; returns whether to send a frame this tick.
    fn tick(&mut self, dev: &mut Dev) -> io::Result<bool> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => self.serve(stream, dev),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(self.streaming)
    }

    fn serve(&mut self, stream: UnixStream, dev: &mut Dev) {
        let mut request = String::new();
        if BufReader::new(&stream).read_line(&mut request).is_err() {
            return; // client went away, not fatal
        }

        let reply = match Cli::try_parse_from(
            std::iter::once("zenvision").chain(request.split_whitespace()),
        ) {
            Ok(cli) => self.apply(cli.command, dev),
            Err(e) => format!("error: {e}"),
        };

        let _ = writeln!(&stream, "{reply}");
    }

    fn apply(&mut self, command: Command, dev: &mut Dev) -> String {
        match command {
            Command::Daemon => "error: daemon already running".into(),
            Command::Demo { .. } => match anim::start(dev) {
                Ok(()) => {
                    self.set_streaming(true);
                    "ok".into()
                }
                Err(e) => format!("error: {e}"),
            },
            command => match execute(dev, &command) {
                Ok(output) => {
                    // Commands that switch the panel to built-in content stop frames.
                    let streaming = match command {
                        Command::Off | Command::On => false,
                        Command::Mode { value } => value == 2,
                        _ => self.streaming,
                    };
                    self.set_streaming(streaming);
                    output.unwrap_or_else(|| "ok".into())
                }
                Err(e) => format!("error: {e}"),
            },
        }
    }

    fn set_streaming(&mut self, on: bool) {
        if self.streaming != on {
            self.streaming = on;
            println!("{}", if on { "streaming" } else { "streaming paused" });
        }
    }
}

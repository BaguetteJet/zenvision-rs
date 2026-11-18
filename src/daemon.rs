use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use clap::Parser;

use crate::animation::{AnimationKind, DEFAULT_FPS};
use crate::device::{Device, open_device};
use crate::player::Player;
use crate::{Cli, Command, StreamEffect, commands, execute};

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
        player: Player::new(AnimationKind::default(), DEFAULT_FPS),
    };
    daemon.run(&mut dev)
}

struct Daemon {
    listener: UnixListener,
    player: Player,
}

impl Daemon {
    fn run(&mut self, dev: &mut Device) -> io::Result<()> {
        loop {
            self.serve_pending(dev)?;
            self.player.step(dev)?;
        }
    }

    // Serve every client that is queued, without blocking the frame loop.
    fn serve_pending(&mut self, dev: &mut Device) -> io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => self.serve(stream, dev),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn serve(&mut self, stream: UnixStream, dev: &mut Device) {
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

    fn apply(&mut self, command: Command, dev: &mut Device) -> String {
        match command {
            Command::Daemon => "error: daemon already running".into(),
            Command::Anim { animation, fps } => {
                let kind = animation.unwrap_or_else(|| self.player.kind().clone());
                let fps = fps.unwrap_or(self.player.fps());
                let changed = !self.player.playing()
                    || kind != *self.player.kind()
                    || fps != self.player.fps();
                let name = kind.name();

                match commands::start_frames(dev) {
                    Ok(()) => {
                        self.player.play(kind, fps);
                        if changed {
                            println!("streaming {name} at {fps} fps");
                        }
                        "ok".into()
                    }
                    Err(e) => format!("error: {e}"),
                }
            }
            command => {
                let effect = command.stream_effect();
                match execute(dev, &command) {
                    Ok(output) => {
                        if let Err(e) = self.apply_stream_effect(dev, effect) {
                            return format!("error: {e}");
                        }
                        output.unwrap_or_else(|| "ok".into())
                    }
                    Err(e) => format!("error: {e}"),
                }
            }
        }
    }

    fn apply_stream_effect(&mut self, dev: &mut Device, effect: StreamEffect) -> io::Result<()> {
        match effect {
            StreamEffect::Keep => {}
            StreamEffect::Play => {
                commands::start_frames(dev)?;
                if !self.player.playing() {
                    self.player.resume();
                    println!(
                        "streaming {} at {} fps",
                        self.player.kind().name(),
                        self.player.fps()
                    );
                }
            }
            StreamEffect::Pause => {
                if self.player.playing() {
                    self.player.pause();
                    println!("streaming paused");
                }
            }
        }
        Ok(())
    }
}

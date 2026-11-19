//! The single-owner event loop.
//!
//! One thread owns the USB device, the player and the persisted state. Small
//! helper threads (IPC, frame producer, hotplug, signals) only push events.
//! While paused or driven by a live producer the loop blocks with no timers,
//! so an idle daemon never wakes up.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use nusb::DeviceId;
use nusb::hotplug::HotplugEvent;
use signal_hook::consts::{SIGINT, SIGTERM};

use crate::animation::{AnimationKind, Status};
use crate::canvas::Canvas;
use crate::device::is_panel;
use crate::event::Event;
use crate::ipc::{self, Command, Paths, Reply};
use crate::panel::{Panel, RealPanel};
use crate::protocol::{BUILTIN_MODE, ContentMode, Fps, PanelCommand};
use crate::render::Player;
use crate::state::State;

const RECONNECT_PERIOD: Duration = Duration::from_secs(2);

/// Run the daemon until SIGINT/SIGTERM.
///
/// # Errors
/// Fails when the sockets cannot be bound or another daemon is running.
pub fn run() -> io::Result<()> {
    let paths = Paths::new()?;
    if ipc::is_running(&paths) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("daemon is already running on {}", paths.control.display()),
        ));
    }

    let state = State::load();
    let player = Player::new(state.animation.clone(), state.fps);

    let (events_tx, events_rx) = mpsc::channel();
    spawn_signals(events_tx.clone())?;
    spawn_hotplug(events_tx.clone());

    let server = ipc::bind(&paths)?;
    let (frame_tx, frame_rx) = mpsc::sync_channel(1);
    let (spare_tx, spare_rx) = mpsc::sync_channel(1);
    server.spawn(events_tx.clone(), frame_tx, spare_rx);

    println!("zenvision daemon on {}", paths.control.display());

    let mut app = App {
        paths,
        state,
        player,
        panel: None,
        device_id: None,
        live: false,
        live_has_frame: false,
        live_canvas: Canvas::new(),
        frame_rx,
        spare_tx,
        last_error: None,
    };
    app.try_open();
    app.run(events_rx)
}

fn spawn_signals(events: mpsc::Sender<Event>) -> io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM])?;
    thread::Builder::new()
        .name("zenvision-signals".into())
        .spawn(move || {
            for _ in signals.forever() {
                let _ = events.send(Event::Shutdown);
            }
        })?;
    Ok(())
}

fn spawn_hotplug(events: mpsc::Sender<Event>) {
    let watch = match nusb::watch_devices() {
        Ok(watch) => watch,
        Err(error) => {
            eprintln!("warning: hotplug unavailable, using retry timer: {error}");
            return;
        }
    };

    let _ = thread::Builder::new()
        .name("zenvision-hotplug".into())
        .spawn(move || {
            futures_lite::future::block_on(async {
                use futures_lite::StreamExt;

                let mut watch = watch;
                while let Some(event) = watch.next().await {
                    let event = match event {
                        HotplugEvent::Connected(info) if is_panel(&info) => Event::DeviceConnected,
                        HotplugEvent::Disconnected(id) => Event::DeviceDisconnected(id),
                        _ => continue,
                    };
                    if events.send(event).is_err() {
                        return;
                    }
                }
            });
        });
}

struct App {
    paths: Paths,
    state: State,
    player: Player,
    panel: Option<RealPanel>,
    device_id: Option<DeviceId>,
    live: bool,
    live_has_frame: bool,
    live_canvas: Canvas,
    frame_rx: Receiver<Vec<u8>>,
    spare_tx: SyncSender<Vec<u8>>,
    last_error: Option<String>,
}

impl App {
    fn run(&mut self, events: Receiver<Event>) -> io::Result<()> {
        loop {
            let event = match self.deadline() {
                Some(deadline) => {
                    let now = Instant::now();
                    match events.recv_timeout(deadline.saturating_duration_since(now)) {
                        Ok(event) => Some(event),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                None => match events.recv() {
                    Ok(event) => Some(event),
                    Err(_) => break,
                },
            };

            match event {
                None => self.on_deadline(),
                Some(Event::Command(incoming)) => {
                    let reply = self.apply(incoming.command);
                    let _ = incoming.reply.send(reply);
                }
                Some(Event::FrameReady) => self.on_frame(),
                Some(Event::Input(input)) => {
                    if let Err(error) = self.player.input(&input) {
                        eprintln!("animation input failed: {error}");
                    }
                }
                Some(Event::ProducerGone) => self.on_producer_gone(),
                Some(Event::DeviceConnected) => self.try_open(),
                Some(Event::DeviceDisconnected(id)) => {
                    if self.device_id == Some(id) {
                        self.panel_failed(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "panel disconnected",
                        ));
                    }
                }
                Some(Event::Shutdown) => {
                    self.shutdown();
                    return Ok(());
                }
            }
        }

        self.shutdown();
        Ok(())
    }

    /// The next timer, or `None` for a fully idle event loop.
    fn deadline(&self) -> Option<Instant> {
        if self.panel.is_none() {
            return Some(Instant::now() + RECONNECT_PERIOD);
        }
        if self.live {
            return None;
        }
        self.player.next_deadline()
    }

    fn on_deadline(&mut self) {
        if self.panel.is_none() {
            self.try_open();
            return;
        }
        if self.live {
            return;
        }

        // Native frames are now the display owner; a later live frame must be
        // uploaded even if it matches the stale live canvas.
        self.live_has_frame = false;
        let result = match self.panel.as_mut() {
            Some(panel) => self.player.tick(panel),
            None => Ok(Status::Unchanged),
        };
        match result {
            Ok(Status::Ended) => {
                self.state.playing = false;
                self.save();
            }
            Ok(_) => {}
            Err(error) => self.panel_failed(error),
        }
    }

    fn on_frame(&mut self) {
        let frame = match self.frame_rx.recv() {
            Ok(frame) => frame,
            Err(_) => return,
        };

        let canvas = &mut self.live_canvas;
        let changed = !self.live_has_frame || frame.as_slice() != canvas.pixels();
        let result = if changed {
            canvas
                .set_grayscale(&frame)
                .and_then(|()| match self.panel.as_mut() {
                    Some(panel) => panel.frame(canvas),
                    None => Ok(()),
                })
        } else {
            Ok(())
        };

        let _ = self.spare_tx.send(frame);

        match result {
            Ok(()) => {
                self.live = true;
                self.live_has_frame = true;
            }
            Err(error) => self.panel_failed(error),
        }
    }

    fn on_producer_gone(&mut self) {
        self.live = false;
        self.live_has_frame = false;
        if self.state.playing {
            self.player.resume();
        } else {
            self.player.pause();
            if let Some(panel) = self.panel.as_mut() {
                let _ = panel.apply(&BUILTIN_MODE);
            }
        }
        println!("frame producer disconnected");
    }

    fn try_open(&mut self) {
        if self.panel.is_some() {
            return;
        }

        let mut panel = match RealPanel::open() {
            Ok(panel) => panel,
            Err(error) => {
                let message = error.to_string();
                if self.last_error.as_deref() != Some(message.as_str()) {
                    eprintln!("waiting for panel: {error}");
                    self.last_error = Some(message);
                }
                return;
            }
        };

        let id = panel.device_id();
        if let Err(error) = self.state.apply(&mut panel, &mut self.player) {
            eprintln!("warning: could not restore panel state: {error}; retrying");
            self.panel_failed(error);
            return;
        }

        self.panel = Some(panel);
        self.device_id = id;
        self.live = false;
        self.live_has_frame = false;
        self.last_error = None;
        println!("panel connected");
    }

    fn apply(&mut self, command: Command) -> Reply {
        match command {
            Command::Play { animation, fps } => self.play(animation, fps),
            Command::Panel(command) => self.apply_panel(command),
        }
    }

    fn play(&mut self, animation: Option<AnimationKind>, fps: Option<Fps>) -> Reply {
        let kind = animation.unwrap_or_else(|| self.player.kind().clone());
        let fps = fps.unwrap_or_else(|| self.player.fps());

        self.player.play(kind.clone(), fps);
        self.state.animation = kind;
        self.state.fps = fps;
        self.state.playing = true;
        self.save();

        if self.live {
            return Reply::message("saved; a live producer owns the display");
        }

        match self.begin_streaming() {
            Ok(()) => Reply::ok(),
            Err(message) => Reply::error(message),
        }
    }

    fn apply_panel(&mut self, command: PanelCommand) -> Reply {
        if matches!(command, PanelCommand::State) {
            return match self.panel_apply(&command) {
                Ok(Some(output)) => Reply::message(output),
                Ok(None) if self.panel.is_none() => Reply::message(self.state.summary()),
                Ok(None) => Reply::ok(),
                Err(message) => Reply::error(message),
            };
        }

        match &command {
            PanelCommand::On => {
                self.state.playing = true;
                self.player.resume();
                self.save();
            }
            PanelCommand::Off => {
                self.state.playing = false;
                self.player.pause();
                self.save();
            }
            PanelCommand::Mode { value } => {
                let streaming = *value == ContentMode::Two;
                self.state.playing = streaming;
                if streaming {
                    self.player.resume();
                } else {
                    self.player.pause();
                }
                self.save();
            }
            PanelCommand::Time { format } => {
                self.state.time_format = *format;
                self.save();
            }
            PanelCommand::Brightness { value } => {
                self.state.brightness = Some(*value);
                self.save();
            }
            _ => {}
        }

        match self.panel_apply(&command) {
            Ok(_) => {
                let wants_streaming = matches!(command, PanelCommand::On)
                    || matches!(
                        command,
                        PanelCommand::Mode {
                            value: ContentMode::Two
                        }
                    );
                if wants_streaming && !self.live {
                    match self.begin_streaming() {
                        Ok(()) => Reply::ok(),
                        Err(message) => Reply::error(message),
                    }
                } else {
                    Reply::ok()
                }
            }
            Err(message) => Reply::error(message),
        }
    }

    fn begin_streaming(&mut self) -> Result<(), String> {
        if self.panel.is_none() || self.live {
            return Ok(());
        }
        self.panel_apply(&PanelCommand::Mode {
            value: ContentMode::Two,
        })?;
        self.panel_apply(&PanelCommand::Sweep { value: false })?;
        Ok(())
    }

    fn panel_apply(&mut self, command: &PanelCommand) -> Result<Option<String>, String> {
        let result = match self.panel.as_mut() {
            Some(panel) => panel.apply(command),
            None => return Ok(None),
        };

        match result {
            Ok(output) => Ok(output),
            Err(error) => {
                let message = format!("panel error: {error}");
                self.panel_failed(error);
                Err(message)
            }
        }
    }

    fn panel_failed(&mut self, error: io::Error) {
        eprintln!("panel lost: {error}; waiting for reconnect");
        self.panel = None;
        self.device_id = None;
        self.live = false;
        self.live_has_frame = false;
    }

    fn save(&self) {
        if let Err(error) = self.state.save() {
            eprintln!("warning: could not save state: {error}");
        }
    }

    fn shutdown(&mut self) {
        if let Some(panel) = self.panel.as_mut()
            && let Err(error) = panel.apply(&BUILTIN_MODE)
        {
            eprintln!("warning: could not restore built-in mode: {error}");
        }

        let _ = std::fs::remove_file(&self.paths.control);
        let _ = std::fs::remove_file(&self.paths.frames);
    }
}

/// Run a native animation in the foreground (used by `anim` without a daemon
/// and by `preview`). The shared [`Player`] keeps this loop as small as the
/// daemon's, and a signal restores the built-in mode.
///
/// # Errors
/// Propagates panel failures.
pub fn run_animation(panel: &mut dyn Panel, mut player: Player) -> io::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&stop))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&stop))?;

    let result = (|| {
        panel.apply(&PanelCommand::Mode {
            value: ContentMode::Two,
        })?;
        panel.apply(&PanelCommand::Sweep { value: false })?;
        player.resume();

        while !stop.load(Ordering::Relaxed) {
            if let Some(deadline) = player.next_deadline() {
                let now = Instant::now();
                if deadline > now {
                    thread::sleep(deadline - now);
                }
            }

            if player.tick(panel)? == Status::Ended {
                break;
            }
        }
        Ok(())
    })();

    let _ = panel.apply(&BUILTIN_MODE);
    result
}

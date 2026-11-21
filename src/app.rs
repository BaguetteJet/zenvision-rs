//! The single-owner event loop.
//!
//! One thread owns the USB device, the player and the persisted state. Helper
//! threads (IPC, frame producer, hotplug, signals) only push events. Live
//! frames are displayed on the loop's own deadline; older pending frames are
//! dropped so a fast producer cannot accumulate latency.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use nusb::DeviceId;
use nusb::hotplug::HotplugEvent;
use signal_hook::consts::{SIGINT, SIGTERM};

use crate::animation::{AnimationKind, Status};
use crate::canvas::Canvas;
use crate::device::is_panel;
use crate::event::Event;
use crate::ipc::{self, Command, FrameMailbox, Paths, Reply};
use crate::panel::{Panel, TeePanel};
use crate::preview::PreviewHub;
use crate::protocol::{BUILTIN_DISPLAY, ContentMode, Fps, PanelCommand, Theme};
use crate::render::Player;
use crate::state::State;

const RECONNECT_PERIOD: Duration = Duration::from_secs(2);
const TIME_SYNC_PERIOD: Duration = Duration::from_secs(3600);
const DRAIN_PERIOD: Duration = Duration::from_millis(100);

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

    let (server, preview_socket) = ipc::bind(&paths)?;
    let preview = PreviewHub::spawn(preview_socket);
    let frames = Arc::new(FrameMailbox::new());
    server.spawn(events_tx, Arc::clone(&frames));

    println!("zenvision daemon on {}", paths.control.display());

    let tee = preview.clone();
    let open = move || Ok(Box::new(TeePanel::open(tee.clone())?) as Box<dyn Panel>);
    let mut app = App::new(
        paths,
        state,
        player,
        open,
        frames,
        Some(preview),
        RECONNECT_PERIOD,
    );
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
            eprintln!("warning: hotplug unavailable, using the retry timer: {error}");
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

/// The newest live frame and its display schedule.
struct Live {
    /// A producer has sent a frame and owns the display while it keeps sending.
    active: bool,
    /// `canvas` holds a frame that was already shown.
    has_frame: bool,
    /// Re-send `canvas` even without a new incoming frame.
    dirty: bool,
    canvas: Canvas,
    next: Instant,
}

impl Live {
    fn new(now: Instant) -> Self {
        Self {
            active: false,
            has_frame: false,
            dirty: false,
            canvas: Canvas::new(),
            next: now,
        }
    }

    fn clear(&mut self) {
        self.active = false;
        self.has_frame = false;
        self.dirty = false;
        self.canvas.clear();
    }
}

struct App<F> {
    paths: Paths,
    state: State,
    player: Player,
    open: F,
    panel: Option<Box<dyn Panel>>,
    device_id: Option<DeviceId>,
    live: Live,
    frames: Arc<FrameMailbox>,
    preview: Option<PreviewHub>,
    last_error: Option<String>,
    next_retry: Instant,
    next_time_sync: Instant,
    reconnect_period: Duration,
    /// Disabled by unit tests so they never touch the real state file.
    persist: bool,
}

impl<F> App<F>
where
    F: FnMut() -> io::Result<Box<dyn Panel>>,
{
    fn new(
        paths: Paths,
        state: State,
        player: Player,
        open: F,
        frames: Arc<FrameMailbox>,
        preview: Option<PreviewHub>,
        reconnect_period: Duration,
    ) -> Self {
        let now = Instant::now();
        Self {
            paths,
            state,
            player,
            open,
            panel: None,
            device_id: None,
            live: Live::new(now),
            frames,
            preview,
            last_error: None,
            next_retry: now,
            next_time_sync: now + TIME_SYNC_PERIOD,
            reconnect_period,
            persist: true,
        }
    }

    fn run(&mut self, events: Receiver<Event>) -> io::Result<()> {
        loop {
            let event = match self.deadline() {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(Instant::now());
                    match events.recv_timeout(timeout) {
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
                Some(event) => {
                    if !self.handle(event) {
                        return Ok(());
                    }
                }
            }
        }

        self.shutdown();
        Ok(())
    }

    /// Handle one event. Returns `false` after a clean shutdown.
    fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Command(incoming) => {
                let reply = self.apply(incoming.command);
                let _ = incoming.reply.send(reply);
            }
            Event::FrameReady => self.on_frame(),
            Event::Input(input) => {
                if let Err(error) = self.player.input(&input) {
                    eprintln!("animation input failed: {error}");
                }
            }
            Event::ProducerGone => self.on_producer_gone(),
            Event::DeviceConnected => self.try_open(),
            Event::DeviceDisconnected(id) => {
                if self.device_id == Some(id) {
                    self.panel_failed(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "panel disconnected",
                    ));
                }
            }
            Event::Shutdown => {
                self.shutdown();
                return false;
            }
        }
        true
    }

    /// The next timer, or `None` when there is nothing to do before an event.
    fn deadline(&self) -> Option<Instant> {
        if self.panel.is_none() {
            let mut deadline = self.next_retry;
            if self.frames.has_frame() || self.live.dirty {
                deadline = deadline.min(self.live.next);
            }
            return Some(deadline);
        }

        let mut deadline = self.next_time_sync;
        if self.frames.has_frame() || self.live.dirty {
            deadline = deadline.min(self.live.next);
        } else if !self.live.active {
            if let Some(next) = self.player.next_deadline() {
                deadline = deadline.min(next);
            }
        }
        Some(deadline)
    }

    fn on_deadline(&mut self) {
        let now = Instant::now();

        if self.panel.is_none() && now >= self.next_retry {
            self.try_open();
        }
        if (self.frames.has_frame() || self.live.dirty) && now >= self.live.next {
            self.display_live();
        }
        if self.panel.is_none() {
            return;
        }

        if now >= self.next_time_sync {
            self.sync_time();
            if self.panel.is_none() {
                return;
            }
        }

        if self.live.active {
            return;
        }

        if self
            .player
            .next_deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            self.tick_player();
        }
    }

    fn tick_player(&mut self) {
        let result = match self.panel.as_mut() {
            Some(panel) => self.player.tick(panel.as_mut()),
            None => return,
        };

        match result {
            Ok(Status::Ended) => {
                let _ = self.play(Some(AnimationKind::default()), None);
            }
            Ok(_) => {}
            Err(error) => self.panel_failed(error),
        }
    }

    fn display_live(&mut self) {
        let frame = self.frames.take();
        let displaying = self.panel.is_some() && self.state.playing;
        let mut result = Ok(());

        if displaying && (frame.is_some() || (self.live.dirty && self.live.has_frame)) {
            if let Some(frame) = frame.as_ref() {
                result = self.live.canvas.set_grayscale(frame);
            }
            if result.is_ok() {
                if let Some(panel) = self.panel.as_mut() {
                    result = panel.frame(&self.live.canvas);
                }
            }
            if result.is_ok() {
                self.live.has_frame = true;
            }
        }

        if let Some(frame) = frame {
            self.frames.recycle(frame);
        }
        self.live.dirty = false;

        // Fixed-deadline scheduling. Advancing the previous deadline avoids
        // accumulating USB and event-loop overhead into the frame rate.
        let interval = if displaying {
            period(self.player.fps())
        } else {
            DRAIN_PERIOD
        };
        self.live.next += interval;
        let now = Instant::now();
        if self.live.next <= now {
            self.live.next = now + interval;
        }

        if let Err(error) = result {
            self.panel_failed(error);
        }
    }

    fn on_frame(&mut self) {
        if !self.live.active {
            self.live.active = true;
            self.live.next = Instant::now();
            self.player.pause();
        }
    }

    fn on_producer_gone(&mut self) {
        let was_active = self.live.active;
        self.frames.clear();
        self.live.clear();
        if self.state.playing {
            self.player.resume();
        } else {
            self.player.pause();
        }
        self.update_preview();
        if was_active {
            println!("frame producer disconnected");
        }
    }

    fn try_open(&mut self) {
        if self.panel.is_some() {
            return;
        }

        let now = Instant::now();
        let mut panel = match (self.open)() {
            Ok(panel) => panel,
            Err(error) => {
                let message = error.to_string();
                if self.last_error.as_deref() != Some(message.as_str()) {
                    eprintln!("waiting for panel: {error}");
                    self.last_error = Some(message);
                }
                self.next_retry = now + self.reconnect_period;
                self.update_preview();
                return;
            }
        };

        let id = panel.device_id();
        if let Err(error) = self.state.apply(panel.as_mut(), &mut self.player) {
            eprintln!("warning: could not restore panel state: {error}; retrying");
            self.next_retry = now + self.reconnect_period;
            self.update_preview();
            return;
        }

        self.panel = Some(panel);
        self.device_id = id;
        self.last_error = None;
        self.next_time_sync = now + TIME_SYNC_PERIOD;

        if self.live.active {
            self.player.pause();
            self.live.dirty = self.live.has_frame;
            self.live.next = now;
        }

        self.update_preview();
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

        if self.live.active {
            self.player.pause();
            self.live.dirty = self.live.has_frame;
            self.live.next = Instant::now();
        }
        self.update_preview();

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

        // Display choices are staged while no panel is connected and replayed
        // on connect. Momentary settings cannot be staged, so say so.
        let staged = match &command {
            PanelCommand::On
            | PanelCommand::Off
            | PanelCommand::Theme { .. }
            | PanelCommand::Clock { .. }
            | PanelCommand::Time { .. }
            | PanelCommand::Brightness { .. }
            | PanelCommand::Speed { .. } => true,
            PanelCommand::Mode { value } => *value == ContentMode::TWO,
            _ => false,
        };
        if self.panel.is_none() && !staged {
            return Reply::error("panel is not connected");
        }

        match &command {
            PanelCommand::On => {
                self.state.power = Some(true);
                self.state.theme = Some(Theme::FOUR);
                self.state.clock = None;
                self.stop_streaming();
            }
            PanelCommand::Off => {
                self.state.power = Some(false);
                self.stop_streaming();
            }
            PanelCommand::Theme { value } => {
                self.state.theme = Some(*value);
                self.state.clock = None;
                self.stop_streaming();
            }
            PanelCommand::Clock { value } => {
                self.state.clock = Some(*value);
                self.state.theme = None;
                self.stop_streaming();
            }
            PanelCommand::Mode { value } if *value == ContentMode::TWO => {
                self.state.playing = true;
                if self.live.active {
                    self.player.pause();
                    self.live.dirty = self.live.has_frame;
                    self.live.next = Instant::now();
                } else {
                    self.player.resume();
                }
            }
            PanelCommand::Mode { .. } => self.stop_streaming(),
            PanelCommand::Time { format } => self.state.time_format = *format,
            PanelCommand::Brightness { value } => self.state.brightness = Some(*value),
            PanelCommand::Speed { value } => self.state.speed = Some(*value),
            PanelCommand::Boot { .. }
            | PanelCommand::Sweep { .. }
            | PanelCommand::Battery { .. }
            | PanelCommand::State => {}
        }
        self.save();
        self.update_preview();

        if matches!(
            command,
            PanelCommand::Mode {
                value: ContentMode::TWO
            }
        ) {
            return match self.begin_streaming() {
                Ok(()) => Reply::ok(),
                Err(message) => Reply::error(message),
            };
        }

        if self.panel.is_none() {
            return Reply::ok();
        }

        match self.panel_apply(&command) {
            Ok(_) => Reply::ok(),
            Err(message) => Reply::error(message),
        }
    }

    fn stop_streaming(&mut self) {
        self.state.playing = false;
        self.player.pause();
        self.live.dirty = false;
    }

    fn begin_streaming(&mut self) -> Result<(), String> {
        if self.panel.is_none() {
            return Ok(());
        }
        self.panel_apply(&PanelCommand::Mode {
            value: ContentMode::TWO,
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

    fn sync_time(&mut self) {
        self.next_time_sync = Instant::now() + TIME_SYNC_PERIOD;
        if let Err(message) = self.panel_apply(&PanelCommand::Time {
            format: self.state.time_format,
        }) {
            eprintln!("warning: clock sync failed: {message}");
        }
    }

    fn panel_failed(&mut self, error: io::Error) {
        if self.panel.is_none() {
            return;
        }
        eprintln!("panel lost: {error}; waiting for reconnect");
        self.panel = None;
        self.device_id = None;
        self.next_retry = Instant::now() + self.reconnect_period;
        self.live.dirty = false;
        self.update_preview();
    }

    /// The preview mirrors host frames only while the panel is connected and
    /// actually receiving them.
    fn update_preview(&self) {
        if let Some(preview) = &self.preview {
            preview.set_streaming(self.state.playing && self.panel.is_some());
        }
    }

    fn save(&self) {
        if !self.persist {
            return;
        }
        if let Err(error) = self.state.save() {
            eprintln!("warning: could not save state: {error}");
        }
    }

    fn shutdown(&mut self) {
        // Only replace host frames. A theme, clock, or powered-off panel is
        // already a built-in display and should be left alone. Stopping the
        // daemon returns the panel to the built-in theme.
        if self.state.playing {
            if let Some(panel) = self.panel.as_mut() {
                let theme = PanelCommand::Theme { value: Theme::FOUR };
                if let Err(error) = panel.apply(&theme) {
                    eprintln!("warning: could not restore built-in theme: {error}");
                }
            }
        }

        let _ = std::fs::remove_file(&self.paths.control);
        let _ = std::fs::remove_file(&self.paths.frames);
        let _ = std::fs::remove_file(&self.paths.preview);
    }
}

fn period(fps: Fps) -> Duration {
    Duration::from_secs_f64(1.0 / f64::from(fps.get()))
}

/// Install a SIGINT/SIGTERM flag shared by the foreground loops.
///
/// # Errors
/// Propagates signal registration failures.
pub fn stop_flag() -> io::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGINT, Arc::clone(&stop))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&stop))?;
    Ok(stop)
}

/// Run a native animation in the foreground (used by `anim` without a daemon
/// and by `preview` when no daemon is running). A signal restores the built-in
/// display.
///
/// # Errors
/// Propagates panel failures.
pub fn run_animation(panel: &mut dyn Panel, mut player: Player) -> io::Result<()> {
    let stop = stop_flag()?;

    let result = (|| {
        panel.apply(&PanelCommand::Mode {
            value: ContentMode::TWO,
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
                let fps = player.fps();
                player.play(AnimationKind::default(), fps);
            }
        }
        Ok(())
    })();

    let _ = panel.apply(&BUILTIN_DISPLAY);
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;
    use crate::animation::starfield;
    use crate::canvas::GRAYSCALE_BYTES;
    use crate::protocol::{Theme, TimeFormat};

    #[derive(Default)]
    struct FakeData {
        commands: Vec<PanelCommand>,
        frames: Vec<Vec<u8>>,
    }

    #[derive(Clone, Default)]
    struct FakePanel(Arc<Mutex<FakeData>>);

    impl FakePanel {
        fn frames(&self) -> usize {
            self.0.lock().unwrap().frames.len()
        }

        fn last_frame(&self) -> Vec<u8> {
            self.0.lock().unwrap().frames.last().unwrap().clone()
        }

        fn commands(&self) -> Vec<PanelCommand> {
            self.0.lock().unwrap().commands.clone()
        }
    }

    impl Panel for FakePanel {
        fn apply(&mut self, command: &PanelCommand) -> io::Result<Option<String>> {
            self.0.lock().unwrap().commands.push(command.clone());
            Ok(None)
        }

        fn frame(&mut self, canvas: &Canvas) -> io::Result<()> {
            self.0.lock().unwrap().frames.push(canvas.pixels().to_vec());
            Ok(())
        }
    }

    type TestOpen = Box<dyn FnMut() -> io::Result<Box<dyn Panel>>>;
    type PanelSlot = Arc<Mutex<Option<FakePanel>>>;
    type TestParts = (App<TestOpen>, Arc<FrameMailbox>, PanelSlot);

    fn test_app(state: State, panel: Option<FakePanel>) -> TestParts {
        let frames = Arc::new(FrameMailbox::new());

        let slot = Arc::new(Mutex::new(panel));
        let open_slot = Arc::clone(&slot);
        let open: TestOpen = Box::new(move || match open_slot.lock().unwrap().clone() {
            Some(panel) => Ok(Box::new(panel) as Box<dyn Panel>),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no panel")),
        });

        let player = Player::new(state.animation.clone(), state.fps);
        let paths = Paths {
            control: PathBuf::from("/nonexistent/zenvision-control"),
            frames: PathBuf::from("/nonexistent/zenvision-frames"),
            preview: PathBuf::from("/nonexistent/zenvision-preview"),
        };
        let mut app = App::new(
            paths,
            state,
            player,
            open,
            Arc::clone(&frames),
            None,
            Duration::from_millis(20),
        );
        app.persist = false;
        (app, frames, slot)
    }

    fn playing_state() -> State {
        State {
            animation: AnimationKind::Starfield(starfield::Options::default()),
            playing: true,
            ..State::default()
        }
    }

    #[test]
    fn reconnect_deadline_does_not_move() {
        let (mut app, _frames, _slot) = test_app(State::default(), None);
        app.try_open(); // fails and schedules the retry

        let first = app.deadline().unwrap();
        thread::sleep(Duration::from_millis(5));
        let second = app.deadline().unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn offline_theme_is_recorded_and_replayed_on_connect() {
        let (mut app, _frames, slot) = test_app(State::default(), None);
        app.try_open();

        let theme = Theme::new(3).unwrap();
        let reply = app.apply(Command::Panel(PanelCommand::Theme { value: theme }));
        assert!(matches!(reply, Reply::Ok { .. }));
        assert_eq!(app.state.theme, Some(theme));
        assert!(!app.state.playing);

        let panel = FakePanel::default();
        *slot.lock().unwrap() = Some(panel.clone());
        app.try_open();

        assert!(
            panel
                .commands()
                .iter()
                .any(|command| matches!(command, PanelCommand::Theme { value } if *value == theme))
        );
    }

    #[test]
    fn offline_momentary_command_is_rejected() {
        let (mut app, _frames, _slot) = test_app(State::default(), None);
        app.try_open();

        let reply = app.apply(Command::Panel(PanelCommand::Boot { value: true }));
        assert!(matches!(reply, Reply::Error { .. }));
    }

    #[test]
    fn live_frames_wait_for_the_loop_deadline() {
        let panel = FakePanel::default();
        let (mut app, frames, _slot) = test_app(playing_state(), Some(panel.clone()));
        app.try_open();
        assert_eq!(panel.frames(), 0);

        frames.put(vec![7u8; GRAYSCALE_BYTES]);
        app.handle(Event::FrameReady);
        assert_eq!(panel.frames(), 0);

        app.live.next = Instant::now();
        app.on_deadline();
        assert_eq!(panel.frames(), 1);
        assert!(panel.last_frame().iter().all(|&pixel| pixel == 7));

        frames.put(vec![9u8; GRAYSCALE_BYTES]);
        app.handle(Event::FrameReady);
        assert_eq!(panel.frames(), 1);

        app.live.next = Instant::now();
        app.on_deadline();
        assert_eq!(panel.frames(), 2);
        assert!(panel.last_frame().iter().all(|&pixel| pixel == 9));
    }

    #[test]
    fn live_deadline_advances_from_the_previous_slot() {
        let panel = FakePanel::default();
        let (mut app, frames, _slot) = test_app(playing_state(), Some(panel.clone()));
        app.try_open();

        let interval = period(app.player.fps());
        app.live.next = Instant::now();
        frames.put(vec![7u8; GRAYSCALE_BYTES]);
        app.handle(Event::FrameReady);
        app.display_live();
        let first = app.live.next;

        frames.put(vec![9u8; GRAYSCALE_BYTES]);
        app.handle(Event::FrameReady);
        app.display_live();
        let second = app.live.next;

        assert_eq!(second.duration_since(first), interval);
    }

    #[test]
    fn live_frame_is_resent_after_reconnect() {
        let panel = FakePanel::default();
        let (mut app, frames, _slot) = test_app(playing_state(), Some(panel.clone()));
        app.try_open();

        frames.put(vec![42u8; GRAYSCALE_BYTES]);
        app.handle(Event::FrameReady);
        app.live.next = Instant::now();
        app.on_deadline();
        assert_eq!(panel.frames(), 1);

        app.panel_failed(io::Error::other("unplugged"));
        app.try_open();
        app.on_deadline();

        assert_eq!(panel.frames(), 2);
        assert!(panel.last_frame().iter().all(|&pixel| pixel == 42));
    }

    #[test]
    fn idle_loop_syncs_the_clock_hourly() {
        let panel = FakePanel::default();
        let (mut app, _frames, _slot) = test_app(State::default(), Some(panel.clone()));
        app.try_open();

        app.next_time_sync = Instant::now();
        app.on_deadline();

        assert!(panel.commands().iter().any(|command| matches!(
            command,
            PanelCommand::Time {
                format: TimeFormat::H24
            }
        )));
        assert!(app.next_time_sync > Instant::now());
    }

    #[test]
    fn shutdown_restores_theme_four_only_while_streaming() {
        let theme = PanelCommand::Theme { value: Theme::FOUR };
        let panel = FakePanel::default();
        let (mut app, _frames, _slot) = test_app(playing_state(), Some(panel.clone()));
        app.try_open();
        app.shutdown();

        assert!(panel.commands().contains(&theme));

        let panel = FakePanel::default();
        let (mut app, _frames, _slot) = test_app(State::default(), Some(panel.clone()));
        app.try_open();
        app.shutdown();

        assert!(!panel.commands().contains(&theme));
    }
}

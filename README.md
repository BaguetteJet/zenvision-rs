# zenvision-rs

Control the ASUS ZenVision OLED panel on Linux.

The daemon owns the panel and animation runtime. External programs only supply pixels, which makes the panel usable from Python or any other process that can write bytes.

## Build & install

Requires Rust 1.85 or newer.

```sh
cargo build --release
install -Dm755 target/release/zenvision ~/.local/bin/zenvision
install -Dm644 systemd/zenvision.service ~/.config/systemd/user/zenvision.service
systemctl --user daemon-reload
systemctl --user enable --now zenvision.service
```

## Usage

```sh
zenvision --help
zenvision on
zenvision theme 4
zenvision speed 2
zenvision time 24
zenvision brightness 160
zenvision anim starfield --stars 40 --trail 40 --speed 1.5 --fps 30
```

`anim` forwards to the daemon when one is running; without a daemon it drives the panel directly and exits on Ctrl-C.

`theme` and `clock` are built-in content and pause any custom animation. Use `anim` (or `mode 2`) to resume custom animation. Stopping the daemon while streaming defaults to theme 4.

`preview` mirrors custom animation frames the panel receives while the daemon is running.

The terminal view runs in the alternate screen using Unicode sextant blocks with true grayscale colors, shrinks to fit smaller terminals, never draws wider than the panel's width, restores the previous screen on exit, and leaves no characters behind.

```sh
# with the daemon: mirror the panel
zenvision preview

# raw frames to stdout for ffplay or mpv; the final "-" means stdin
zenvision preview --raw | ffplay -f rawvideo -pixel_format gray -video_size 256x64 -framerate 30 -
```

## Custom animations

### Socket (LIVE)

Connect to `$XDG_RUNTIME_DIR/zenvision-frames.sock` with `AF_UNIX`/`SOCK_SEQPACKET`. **One message is one frame.** Disconnecting (or ten seconds without a frame) releases the display and the daemon resumes its native animation.

Host frames are only shown while the daemon is streaming. Start it with `anim`, `stream`, or `mode 2`.

The contract is deliberately small:

- first byte is a tag: `0` for a frame, `1` for animation input
- frames are raw 8-bit grayscale, 256 × 64, row-major, one byte per pixel
- a frame message is 16,385 bytes including the tag
- input messages are tag + payload and are forwarded to the active animation

### Pipe

Anything that writes bytes works. Frames are paced by the daemon. If the producer sends faster, older frames are dropped:

```sh
python anim.py | zenvision stream --fps 30
ffmpeg -stream_loop -1 -i clip.gif -vf scale=256:64,format=gray -f rawvideo - | zenvision stream
```

`stream` needs the daemon (it forwards tagged frames to the frame socket). The daemon never executes external commands.

### Native Rust animations

Create a module under `src/animation/` exporting an `Options` type that derives serde and `clap::Args`, plus a `<Name>::new(Options)` constructor returning a type implementing `Animation`. The command-line flags, defaults, help text and persisted JSON all come from `Options`, so the animation owns its tuning and there is nothing to mirror elsewhere.

Then add one line to the `animations!` list in `src/animation.rs`:

```rust
my_animation => MyAnimation
```

That line registers the module, the serde tag, the clap subcommand, the factory and the display name together. `Animation::draw` returns `Status::{New, Unchanged, Ended}` and may return an error, so animations can skip redundant USB transfers, finish, or fail. `Animation::input` receives short socket messages for reactive effects.

## Persistence

The desired state is stored at `$XDG_STATE_HOME/zenvision/state.json` (or `~/.local/state/zenvision/state.json`) and re-applied after suspend, replug or daemon restart. It remembers the animation and its options, FPS, playing state, brightness, time format, theme, clock style, built-in speed and power state. A fresh install starts paused and leaves the panel's built-in display alone until the first `anim` or `mode 2`.

## Architecture

```
protocol.rs   validated commands -> USB payload bytes (pure)
device.rs     USB transport only
panel.rs      Panel trait: RealPanel + TerminalPanel
canvas.rs     grayscale canvas + 4bpp wire encoding
animation.rs  Animation trait + built-in animation enum
render.rs     Player: pacing, deadlines, frame timing
preview.rs    shared latest frame + terminal/raw followers
state.rs      persisted desired state
ipc.rs        versioned JSON control socket + tagged seqpacket frame socket
event.rs      events consumed by the loop
app.rs        single-owner event loop, hotplug, signals, reconnect
cli.rs        clap at the edge; preview/stream
```

The event loop is the only owner of the USB device. Helper threads push events; the loop sleeps until the next player frame, retry timer, pending live frame, or hourly clock sync, and keeps no frame timer while paused. A stalled IPC client has its own thread, a read timeout and a 64 KiB request cap, so it cannot block the panel. A frame producer is paced by the loop: its next write blocks until the current frame was displayed, and a ten-second silence releases the display.

## Permissions

If access is denied, add a udev rule:

```sh
# /etc/udev/rules.d/99-zenvision.rules
SUBSYSTEM=="usb", ATTR{idVendor}=="0b05", ATTR{idProduct}=="8835", MODE="0660", TAG+="uaccess"
```

Then reload the rules:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## References

[ZenVision Studio · BaguetteJet EDITION](https://github.com/BaguetteJet/zenvision-studio) - (Python) previous feature rich project   
[zenvision-protocol-research](https://github.com/BaguetteJet/zenvision-protocol-research) - protocol derived from USB packet captures   
[zenvision-linux](https://github.com/tarpediem/zenvision-linux) - first open-source Linux driver

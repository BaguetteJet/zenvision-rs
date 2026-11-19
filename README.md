# zenvision-rs

Control the ASUS ZenVision OLED panel on Linux.

The daemon owns the panel, the clock and the animation. External programs only
supply pixels, which keeps the hot path allocation-free and makes the panel
usable from Python, ffmpeg, or any other process that can write bytes.

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
zenvision state
zenvision anim starfield --stars 40 --trail 40 --speed 1.5 --fps 30
```

`anim` forwards to the daemon when one is running; without a daemon it drives
the panel directly and restores the built-in clock on Ctrl-C.

`theme` and `clock` are panel-owned displays: they pause the animation and
hand the panel over to its built-in theme or clock. Host frames are not sent
over them. Use `anim` (or `mode 2`) to resume streaming afterwards. These
choices are recorded even while no panel is connected and replayed on the next
connect; commands that cannot be staged (such as `boot`) fail instead. Stopping
the daemon while streaming restores the built-in clock instead of leaving the
last frame on screen; an already-selected theme or clock is left alone.

`preview` mirrors the actual frames the panel receives while the daemon is
running, so `anim` option changes, live Python frames, and pauses for
`theme`/`clock` show up immediately. When the panel stops streaming, the
preview stops too and shows a status line. Without a daemon it renders the
requested animation locally. Any number of preview clients can attach at once
and they all read the same latest frame.

The terminal view runs in the alternate screen using Unicode sextant blocks
with true grayscale colors, shrinks to fit smaller terminals, never draws wider
than the panel's 128 cells, restores the previous screen on exit, and leaves no
characters behind:

```sh
# with the daemon: mirror the panel
zenvision preview

# raw frames to stdout for ffplay or mpv; the final "-" means stdin
zenvision preview --raw | ffplay -f rawvideo -pixel_format gray -video_size 256x64 -framerate 30 -

# without a daemon: local animation preview
zenvision preview starfield --fps 15
```

## Custom animations

### Socket (live, low latency)

Connect to `$XDG_RUNTIME_DIR/zenvision-frames.sock` with
`AF_UNIX`/`SOCK_SEQPACKET`. **One message is one frame.** Disconnecting (or
ten seconds without a frame) releases the display and the daemon resumes its
native animation.

Host frames are only shown while the daemon is streaming. Start it with `anim`,
`stream`, or `mode 2`; a fresh daemon starts paused and a selected theme or
clock keeps ownership until then.

The contract is deliberately small:

- first byte is a tag: `0` for a frame, `1` for animation input
- frames are raw 8-bit grayscale, 256 × 64, row-major, one byte per pixel
- a frame message is 16,385 bytes including the tag
- input messages are tag + payload and are forwarded to the active animation

The daemon recycles buffers and keeps at most one frame in flight. It accepts
the next message only after displaying the current frame, so a fast producer
blocks in `send` instead of building a queue or latency. Malformed and
oversized messages are dropped.

```python
import os, socket, tempfile, time
import numpy as np

W, H = 256, 64

def run(draw):
    runtime = os.environ.get("XDG_RUNTIME_DIR") or os.path.join(
        tempfile.gettempdir(), f"zenvision-{os.geteuid()}"
    )
    s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    s.connect(os.path.join(runtime, "zenvision-frames.sock"))

    frame = np.zeros((H, W), np.uint8)
    t0 = time.monotonic()
    while True:
        draw(frame, time.monotonic() - t0)   # mutate in place
        s.send(b"\0" + frame.tobytes())      # blocks until the daemon is ready
```

### Pipe

Anything that writes bytes works. Frames are paced by the daemon; `--fps`
changes the daemon's display rate rather than making the client sleep:

```sh
python anim.py | zenvision stream --fps 30
ffmpeg -i clip.gif -vf scale=256:64,format=gray -f rawvideo - | zenvision stream
```

`stream` needs the daemon (it forwards tagged frames to the frame socket). The
daemon never executes external commands; it only receives pixels.

### Native Rust animations

Create a module under `src/animation/` exporting an `Options` type that derives
serde and `clap::Args`, plus a `<Name>::new(Options)` constructor returning a
type implementing `Animation`. The command-line flags, defaults, help text and
persisted JSON all come from `Options`, so the animation owns its tuning and
there is nothing to mirror elsewhere.

Then add one line to the `animations!` list in `src/animation.rs`:

```rust
my_animation => MyAnimation
```

That line registers the module, the serde tag, the clap subcommand, the factory
and the display name together. `Animation::draw` returns
`Status::{New, Unchanged, Ended}` and may return an error, so animations can
skip redundant USB transfers, finish, or fail. `Animation::input` receives short
socket messages for reactive effects.

## Persistence

The desired state is stored at
`$XDG_STATE_HOME/zenvision/state.json` (or
`~/.local/state/zenvision/state.json`) and re-applied after suspend, replug or
daemon restart. It remembers the animation and its options, FPS, playing
state, brightness, time format, theme, clock style, built-in speed and power
state. A fresh install starts paused and leaves the panel's built-in display
alone until the first `anim` or `mode 2`.

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

The event loop is the only owner of the USB device. Helper threads push events;
the loop sleeps until the next player frame, retry timer, pending live frame, or
hourly clock sync, and keeps no frame timer while paused. A stalled IPC client
has its own thread, a read timeout and a 64 KiB request cap, so it cannot block
the panel. A frame producer is paced by the loop: its next write blocks until
the current frame was displayed, and a ten-second silence releases the display.

The control socket speaks one versioned JSON request and one JSON reply. The
version is checked from the envelope before the command shape is parsed, so
upgrading the binary while an old daemon is running produces a clear version
error instead of a misparse.

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

# zenvision-rs

Control the ASUS ZenVision OLED panel on Linux.

The daemon owns the panel, the clock and the animation. External programs only
supply pixels, which keeps the hot path allocation-free and makes the panel
usable from Python, ffmpeg, or any other process that can write bytes.

## Build & install

Requires Rust 1.88 or newer.

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
over them. Use `anim` (or `mode 2`) to resume streaming afterwards. Stopping
the daemon while streaming restores the built-in clock instead of leaving the
last frame on screen; an already-selected theme or clock is left alone.

`preview` mirrors the actual frames the panel receives while the daemon is
running, so `anim` option changes, live Python frames, and pauses for
`theme`/`clock` show up immediately. When the panel stops streaming, the
preview stops too and shows a status line. Without a daemon it renders the
requested animation locally.

It runs in the terminal's alternate screen as Unicode sextant blocks with true
grayscale colors, scales to the terminal size, restores the previous screen on
exit, and leaves no characters behind:

```sh
# with the daemon: mirror the panel
zenvision preview

# without a daemon: local animation preview
zenvision preview starfield --fps 15
```

## Custom animations

### Socket (live, low latency)

Connect to `$XDG_RUNTIME_DIR/zenvision-frames.sock` with
`AF_UNIX`/`SOCK_SEQPACKET`. **One message is one frame.** Disconnecting releases
the display and the daemon resumes its native animation.

The contract is deliberately small:

- raw 8-bit grayscale
- 256 × 64 pixels
- 16,384 bytes per message
- row-major, one byte per pixel

The daemon recycles two buffers and keeps at most one frame in flight, so a
producer that runs faster than the panel naturally blocks (no queue, no growing
latency). Messages shorter than 16,384 bytes are forwarded to the active
animation's `input` hook.

```python
import os, socket, time
import numpy as np

W, H = 256, 64

def run(draw, fps=30):
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/tmp/zenvision-{os.geteuid()}")
    s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    s.connect(os.path.join(runtime, "zenvision-frames.sock"))

    frame = np.zeros((H, W), np.uint8)
    t0, n = time.monotonic(), 0
    while True:
        draw(frame, time.monotonic() - t0)   # mutate in place
        s.send(frame.tobytes())              # blocks until the daemon is ready
        n += 1
        time.sleep(max(0.0, t0 + n / fps - time.monotonic()))
```

### Pipe

Anything that writes bytes works. Frames are paced by the daemon:

```sh
python anim.py | zenvision stream --fps 30
ffmpeg -i clip.gif -vf scale=256:64,format=gray -f rawvideo - | zenvision stream
```

`stream` needs the daemon (it forwards to the frame socket). The daemon never
executes external commands; it only receives pixels.

### Native Rust animations

Create a module under `src/animation/` with an `Options` type that derives serde
and `clap::Args`, plus a type implementing `Animation`. The command-line flags,
defaults, help text and persisted JSON all come from `Options`, so the animation
owns its tuning and there is nothing to mirror elsewhere.

Then add one variant to `AnimationKind` in `src/animation.rs`. That enum is the
whole registration: it is the serde tag, the clap subcommand and the factory
switch at once. `Animation::draw` returns `Status::{New, Unchanged, Ended}` and
may return an error, so animations can skip redundant USB transfers, finish, or
fail. `Animation::input` receives short socket messages for reactive effects.

## Persistence

The desired state is stored at
`$XDG_STATE_HOME/zenvision/state.json` (or
`~/.local/state/zenvision/state.json`) and re-applied after suspend, replug or
daemon restart. It remembers the animation and its options, FPS, playing
state, brightness and time format.

## Architecture

```
protocol.rs   validated commands -> USB payload bytes (pure)
device.rs     USB transport only
panel.rs      Panel trait: RealPanel + TerminalPanel
canvas.rs     grayscale canvas + 4bpp wire encoding
animation.rs  Animation trait + built-in animation enum
render.rs     Player: pacing, deadlines, frame timing
preview.rs    live frame fan-out + terminal follower
state.rs      persisted desired state
ipc.rs        versioned JSON control socket + seqpacket frame socket
event.rs      events consumed by the loop
app.rs        single-owner event loop, hotplug, signals, reconnect
cli.rs        clap at the edge; preview/stream
```

The event loop is the only owner of the USB device. Helper threads push events;
the loop sleeps with no timer while paused or while a live producer is
connected, and it only wakes for a frame or a command in those states. A stalled
IPC client has its own thread, a read timeout and a 64 KiB request cap, so it
cannot block the panel.

The control socket speaks one versioned JSON request and one JSON reply
(`PROTOCOL_VERSION` in `ipc.rs`); upgrading the binary while an old daemon is
running produces a clear version error instead of a misparse.

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

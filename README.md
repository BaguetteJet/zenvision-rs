# zenvision-rs

Control ASUS ZenVision OLED panel on linux

## Build

```sh
cargo build --release
```

The binary is `target/release/zenvision`

## Usage

```sh
./target/release/zenvision --help
./target/release/zenvision state
./target/release/zenvision time 24
./target/release/zenvision theme 2
./target/release/zenvision on
./target/release/zenvision off
./target/release/zenvision demo
```

## Daemon

`daemon` owns the panel in the background and sets its clock once on start. It
starts idle: run `demo` to start the starfield, and the daemon keeps streaming
it until a command takes the panel back. Commands are forwarded over a Unix
socket (`$XDG_RUNTIME_DIR/zenvision.sock`) when a daemon is running, otherwise
they talk to the panel directly.

```sh
./target/release/zenvision brightness 80
./target/release/zenvision theme 2
./target/release/zenvision off      # powers off and pauses the animation
./target/release/zenvision demo     # resumes the animation
./target/release/zenvision state
```

Stop the daemon with Ctrl-C (or `kill`). The socket is recreated on the next
start, so a stale socket file is harmless.

## Run as a service

A user unit is included in `systemd/zenvision.service`. Install it with:

```sh
cargo build --release
install -Dm755 target/release/zenvision ~/.local/bin/zenvision
install -Dm644 systemd/zenvision.service ~/.config/systemd/user/zenvision.service
systemctl --user daemon-reload
systemctl --user enable --now zenvision.service
```

```sh
systemctl --user status zenvision.service
journalctl --user -u zenvision.service -f
systemctl --user restart zenvision.service
systemctl --user stop zenvision.service
```

The daemon sets the panel clock once when it starts (24-hour format). To keep
it running after logout, enable lingering once:
`loginctl enable-linger "$USER"`.

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

# zenvision-rs

Control ASUS ZenVision OLED panel on linux

## Usage

```sh
zenvision --help
zenvision on
zenvision off
zenvision state
zenvision time 24
zenvision theme 4
zenvision speed 2
zenvision brightness 80
zenvision demo starfield --stars 90 --trail 66 --fps 30
```

`demo [ANIMATION]` streams host-rendered animations. Run `zenvision demo --help`
or `zenvision demo starfield --help` for the available options.

## Build

```sh
cargo build --release
```

The binary is `target/release/zenvision`

## Daemon

`daemon` owns the panel in the background and sets its clock once on start. Commands are forwarded over a Unix socket (`$XDG_RUNTIME_DIR/zenvision.sock`) when a daemon is running, otherwise they talk to the panel directly.

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

To keep it running after logout, enable lingering once: `loginctl enable-linger "$USER"`.

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

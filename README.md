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
```

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

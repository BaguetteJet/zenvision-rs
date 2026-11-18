# zenvision-rs

Control ASUS ZenVision OLED panel on linux

## Build Service

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
zenvision anim starfield --stars 90 --trail 66 --speed 1.5 --fps 30
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

## References

[ZenVision Studio · BaguetteJet EDITION](https://github.com/BaguetteJet/zenvision-studio) - (Python) previous feature rich project   
[zenvision-protocol-research](https://github.com/BaguetteJet/zenvision-protocol-research) - protocol derived from USB packet captures   
[zenvision-linux](https://github.com/tarpediem/zenvision-linux) - first open-source Linux driver

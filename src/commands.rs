use std::io;

use chrono::{Datelike, Local, Timelike};

use crate::device::{Device, send_command};

// 30 05 01 <clock> set clock display layout
pub fn set_clock(dev: &mut Device, clock: u8) -> io::Result<()> {
    assert!((1..=2).contains(&clock), "clock must be 1-2");
    send_command(dev, &[0x30, 0x05, 0x01, clock])?;
    Ok(())
}

// 30 05 02 00 <theme> set built-in theme
pub fn set_theme(dev: &mut Device, theme: u8) -> io::Result<()> {
    assert!((1..=4).contains(&theme), "theme must be 1-4");
    send_command(dev, &[0x30, 0x05, 0x02, 0x00, theme])?;
    Ok(())
}

// 30 05 04 00 00 00 <val> set battery icon
pub fn set_battery(dev: &mut Device, battery: bool) -> io::Result<()> {
    let value = if battery { 0x03 } else { 0x01 };
    send_command(dev, &[0x30, 0x05, 0x04, 0x00, 0x00, 0x00, value])?;
    Ok(())
}

// 30 06 05 00 00 00 00 <mode> set content mode
pub fn set_content_mode(dev: &mut Device, mode: u8) -> io::Result<()> {
    assert!((1..=3).contains(&mode), "mode must be 1-3");
    send_command(dev, &[0x30, 0x06, 0x05, 0x00, 0x00, 0x00, 0x00, mode])?;
    Ok(())
}

// 31 02 <a> <b> set screen sweep
pub fn set_screen_sweep(dev: &mut Device, sweep: bool) -> io::Result<()> {
    let (a, b) = if sweep { (0x02, 0x03) } else { (0x00, 0x04) };
    send_command(dev, &[0x31, 0x02, a, b])?;
    Ok(())
}

// 32 02 <a> <b> boot animation on/off
pub fn set_boot_animation(dev: &mut Device, on: bool) -> io::Result<()> {
    let value = if on { 0x02 } else { 0x00 };
    send_command(dev, &[0x32, 0x02, value, value])?;
    Ok(())
}

// 33 01 <speed> set animation speed
pub fn set_speed(dev: &mut Device, speed: u8) -> io::Result<()> {
    assert!((1..=3).contains(&speed), "speed must be 1-3");
    send_command(dev, &[0x33, 0x01, speed])?;
    Ok(())
}

// 35 01 <brightness> set brightness
pub fn set_brightness(dev: &mut Device, value: u8) -> io::Result<()> {
    send_command(dev, &[0x35, 0x01, value])?;
    Ok(())
}

// 40 09 <datetime> <format> <weekday> set date and time
pub fn set_time(dev: &mut Device, use_24h: bool) -> io::Result<()> {
    let now = Local::now();
    let mut payload = vec![0x40, 0x09];

    payload.extend_from_slice(&(now.year() as u16).to_le_bytes());
    payload.push(now.month() as u8);
    payload.push(now.day() as u8);
    payload.push(now.hour() as u8);
    payload.push(now.minute() as u8);
    payload.push(now.second() as u8);
    payload.push(use_24h as u8);
    payload.push(now.weekday().num_days_from_sunday() as u8);

    send_command(dev, &payload)?;
    Ok(())
}

// f1 03 query content state: "01" clock, "02" theme, "07" image
pub fn get_engine_state(dev: &mut Device) -> io::Result<String> {
    let reply = send_command(dev, &[0xf1, 0x03])?.unwrap_or_default();
    Ok(String::from_utf8_lossy(&reply)
        .trim_end_matches('\0')
        .to_string())
}

// Power off
pub fn power_off(dev: &mut Device) -> io::Result<()> {
    send_command(dev, &[0x30, 0x05, 0x04, 0x00, 0x00, 0x00, 0x00])?; // power off
    send_command(dev, &[0x31, 0x02, 0x00, 0x04])?; // screen sweep off
    Ok(())
}

// Power on
pub fn power_on(dev: &mut Device) -> io::Result<()> {
    send_command(dev, &[0x30, 0x05, 0x04, 0x00, 0x00, 0x00, 0x03])?; // battery on
    send_command(dev, &[0x30, 0x05, 0x02, 0x00, 0x04])?; // theme 4
    Ok(())
}

// Prepare panel for streaming frames
pub fn start_frames(dev: &mut Device) -> io::Result<()> {
    set_content_mode(dev, 2)?;
    set_screen_sweep(dev, false)
}

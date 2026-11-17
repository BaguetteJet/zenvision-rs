use std::io;
use std::time::Duration;

use nusb::transfer::{In, Interrupt, Out, TransferError};
use nusb::{Endpoint, Interface, MaybeFuture};

const VENDOR_ID: u16 = 0x0b05;
const PRODUCT_ID: u16 = 0x8835;

const COMMAND_EP: u8 = 0x03; // interrupt OUT, command channel
const FRAMEBUFFER_EP: u8 = 0x07; // bulk OUT, framebuffer (not used yet)
const RESPONSE_EP: u8 = 0x82; // interrupt IN, response channel

const TIMEOUT: Duration = Duration::from_secs(3);

pub struct Dev {
    command: Endpoint<Interrupt, Out>,
    response: Endpoint<Interrupt, In>,
    _interface: Interface,
}

// Open the panel and claim its vendor interface.
pub fn open_device() -> io::Result<Dev> {
    let info = nusb::list_devices()
        .wait()?
        .find(|d| d.vendor_id() == VENDOR_ID && d.product_id() == PRODUCT_ID)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ZenVision panel not found"))?;

    let device = info.open().wait()?;
    let interface = device.detach_and_claim_interface(0).wait()?;

    let command = interface.endpoint::<Interrupt, Out>(COMMAND_EP)?;
    let response = interface.endpoint::<Interrupt, In>(RESPONSE_EP)?;

    Ok(Dev {
        command,
        response,
        _interface: interface,
    })
}

// Send a command zero-padded to 512 bytes, read the reply on EP 0x82.
// Returns None if the panel does not answer before the timeout.
pub fn send_cmd(dev: &mut Dev, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
    assert!(payload.len() <= 512, "command payload exceeds 512 bytes");

    let mut buf = vec![0u8; 512];
    buf[..payload.len()].copy_from_slice(payload);

    let sent = dev.command.transfer_blocking(buf.into(), TIMEOUT);
    match sent.status {
        Ok(()) => {}
        Err(TransferError::Cancelled) => return Ok(None),
        Err(e) => return Err(e.into()),
    }

    let reply = dev.response.allocate(512);
    let received = dev.response.transfer_blocking(reply, TIMEOUT);
    match received.status {
        Ok(()) => Ok(Some(received.buffer[..received.actual_len].to_vec())),
        Err(TransferError::Cancelled) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

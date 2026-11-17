use std::io;
use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Interrupt, Out, TransferError};
use nusb::{Endpoint, Interface, MaybeFuture};

const VENDOR_ID: u16 = 0x0b05;
const PRODUCT_ID: u16 = 0x8835;

const COMMAND_EP: u8 = 0x03; // interrupt OUT, command channel
const FRAMEBUFFER_EP: u8 = 0x07; // bulk OUT, framebuffer
const RESPONSE_EP: u8 = 0x82; // interrupt IN, response channel

const TIMEOUT: Duration = Duration::from_secs(3);
pub const FRAME_BYTES: usize = 8704;

pub struct Dev {
    command: Endpoint<Interrupt, Out>,
    framebuffer: Endpoint<Bulk, Out>,
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
    let framebuffer = interface.endpoint::<Bulk, Out>(FRAMEBUFFER_EP)?;
    let response = interface.endpoint::<Interrupt, In>(RESPONSE_EP)?;

    Ok(Dev {
        command,
        framebuffer,
        response,
        _interface: interface,
    })
}

// Send one 8704-byte framed image on EP 0x07, giving the buffer back for reuse.
pub fn send_frame(dev: &mut Dev, frame: Buffer) -> io::Result<Buffer> {
    assert_eq!(frame.len(), FRAME_BYTES, "invalid frame size");

    dev.framebuffer
        .transfer_blocking(frame, TIMEOUT)
        .into_result()
        .map_err(io::Error::from)
}

// Send a command zero-padded to 512 bytes, read the reply on EP 0x82.
// Returns None if the panel does not answer before the timeout.
pub fn send_command(dev: &mut Dev, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
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

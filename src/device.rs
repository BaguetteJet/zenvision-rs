//! USB transport for the panel. This is the only module that touches hardware.

use std::io;
use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Interrupt, Out, TransferError};
use nusb::{DeviceId, DeviceInfo, Endpoint, Interface, MaybeFuture};

use crate::canvas::WIRE_FRAME_BYTES;
use crate::protocol::MAX_PAYLOAD;

const VENDOR_ID: u16 = 0x0b05;
const PRODUCT_ID: u16 = 0x8835;

const COMMAND_EP: u8 = 0x03; // interrupt OUT, command channel
const FRAMEBUFFER_EP: u8 = 0x07; // bulk OUT, framebuffer
const RESPONSE_EP: u8 = 0x82; // interrupt IN, response channel

const TIMEOUT: Duration = Duration::from_secs(3);

/// A send failure that still returns the buffer for reuse.
pub type FrameResult = Result<Buffer, (io::Error, Buffer)>;

pub struct Device {
    command: Endpoint<Interrupt, Out>,
    framebuffer: Endpoint<Bulk, Out>,
    response: Endpoint<Interrupt, In>,
    id: DeviceId,
    _interface: Interface,
}

/// Whether a USB device is the panel this crate drives.
pub fn is_panel(info: &DeviceInfo) -> bool {
    info.vendor_id() == VENDOR_ID && info.product_id() == PRODUCT_ID
}

/// Open the panel and claim its vendor interface.
///
/// # Errors
/// Fails when the panel is absent, permission is denied, or the interface
/// cannot be claimed.
pub fn open_device() -> io::Result<Device> {
    let info = nusb::list_devices()
        .wait()?
        .find(is_panel)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ZenVision panel not found"))?;

    let id = info.id();
    let device = info.open().wait()?;
    let interface = device.detach_and_claim_interface(0).wait()?;

    let command = interface.endpoint::<Interrupt, Out>(COMMAND_EP)?;
    let framebuffer = interface.endpoint::<Bulk, Out>(FRAMEBUFFER_EP)?;
    let response = interface.endpoint::<Interrupt, In>(RESPONSE_EP)?;

    Ok(Device {
        command,
        framebuffer,
        response,
        id,
        _interface: interface,
    })
}

impl Device {
    pub fn id(&self) -> DeviceId {
        self.id
    }

    /// Allocate a frame buffer ready for [`Device::send_frame`].
    pub fn frame_buffer() -> Buffer {
        Buffer::from(vec![0u8; WIRE_FRAME_BYTES])
    }

    /// Send one framed image on EP 0x07.
    ///
    /// The buffer is returned on both success and failure so the caller can
    /// reuse it; dropping it on error would be a latent panic.
    pub fn send_frame(&mut self, frame: Buffer) -> FrameResult {
        let completion = self.framebuffer.transfer_blocking(frame, TIMEOUT);
        match completion.status {
            Ok(()) => Ok(completion.buffer),
            Err(error) => Err((error.into(), completion.buffer)),
        }
    }

    /// Send a command zero-padded to one packet, then read the reply on EP 0x82.
    ///
    /// Returns `None` if the panel does not answer before the timeout.
    ///
    /// # Errors
    /// Fails when the payload exceeds the command packet, or on a USB error.
    pub fn send_command(&mut self, payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
        if payload.len() > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("command payload exceeds {MAX_PAYLOAD} bytes"),
            ));
        }

        let mut buf = vec![0u8; MAX_PAYLOAD];
        buf[..payload.len()].copy_from_slice(payload);

        let sent = self.command.transfer_blocking(buf.into(), TIMEOUT);
        match sent.status {
            Ok(()) => {}
            // A timed-out command transfer means the command channel is wedged;
            // surface it so the daemon drops the device and reconnects.
            Err(TransferError::Cancelled) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "panel command transfer timed out",
                ));
            }
            Err(error) => return Err(error.into()),
        }

        let reply = self.response.allocate(MAX_PAYLOAD);
        let received = self.response.transfer_blocking(reply, TIMEOUT);
        match received.status {
            Ok(()) => Ok(Some(received.buffer[..received.actual_len].to_vec())),
            Err(TransferError::Cancelled) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

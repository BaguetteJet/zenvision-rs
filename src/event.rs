//! Events delivered to the single-owner loop.

use nusb::DeviceId;

use crate::ipc::Incoming;

pub enum Event {
    /// A control-socket request waiting for a reply.
    Command(Incoming),
    /// A frame is available on the frame channel.
    FrameReady,
    /// A short control message for the active animation.
    Input(Vec<u8>),
    /// The frame producer disconnected.
    ProducerGone,
    /// A matching USB device appeared.
    DeviceConnected,
    /// A USB device disappeared.
    DeviceDisconnected(DeviceId),
    /// SIGINT or SIGTERM.
    Shutdown,
}

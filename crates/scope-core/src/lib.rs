//! Protocol core for the AVEO Cheetah3 USB microscope (USB ID `1871:01b0`),
//! sold as the Traveller 6803 and under other brands.
//!
//! This crate has no I/O. It describes *what* to send to the device and
//! how to interpret what comes back, so the same logic can drive WebUSB in
//! the browser, a native libusb/nusb tool, or unit tests.
//!
//! Protocol knowledge comes from kswit's reverse-engineering work
//! (<https://github.com/kswit/USB_driver>, MIT licence).

pub mod color;
pub mod firmware;
pub mod frame;
pub mod patch;
pub mod protocol;

pub use color::{yuv422_to_rgba, ByteOrder};
pub use firmware::{Firmware, FirmwareError};
pub use frame::{FrameAssembler, FrameStats};
pub use protocol::{Control, Request, Resolution};

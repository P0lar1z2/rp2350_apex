//! Pure Rust USB 1.1 host protocol primitives.
//!
//! The RP2350 PIO backend feeds and consumes these packets; this module has no
//! dependency on a particular microcontroller.

mod crc;
mod descriptor;
mod device_clone;
mod hid_report;
mod packet;
mod tx;

pub use crc::{crc5_token, crc16_step, crc16_usb};
pub use descriptor::{
    DescriptorError, HidConfiguration, HidEndpoint, HidKind, MAX_HID_INTERFACES,
    parse_hid_configuration,
};
pub use device_clone::{
    CloneDescriptorError, CloneEndpoint, CloneIdentity, CloneInterface, CloneProfile, CloneString,
    MAX_CLONE_INTERFACES, MAX_CLONE_STRING_BYTES, MAX_REPORT_DESCRIPTOR_BYTES,
    parse_clone_configuration,
};
pub use hid_report::{
    DecodedReport, KEY_BITMAP_BYTES, KeyboardState, MouseState, ReportDecoder, ReportEncodeError,
    parse_report_descriptor,
};
pub use packet::{
    DATA_PACKET_OVERHEAD, MAX_PACKET_BYTES, PID_ACK, PID_DATA0, PID_DATA1, PID_IN, PID_NAK,
    PID_OUT, PID_SETUP, PID_SOF, PID_STALL, PacketError, ReceivedPacket, SetupPacket, UsbPid,
    build_data_packet, build_sof_packet, build_token_packet, parse_received_packet, pid_is_valid,
};
pub use tx::{EncodeError, LineSymbol, encode_tx_packet};

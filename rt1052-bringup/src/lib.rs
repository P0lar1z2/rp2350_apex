#![no_std]

#[path = "../../src/usb_host/hid_report.rs"]
pub mod hid_report;

#[cfg(feature = "nxp-device")]
pub mod usb_host {
    pub use crate::hid_report::*;
}

#[cfg(feature = "nxp-device")]
#[path = "../../src/hid_device.rs"]
pub mod hid_device;

/// Marks firmware that relies on the RT1052 reset-time default RAM partition.
///
/// Keeping this call in binaries also retains this crate's native-link
/// metadata when the `nxp-host` feature builds the NXP USB C archive.
#[inline(always)]
pub fn use_nxp_default_flexram() {}

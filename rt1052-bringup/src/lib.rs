#![no_std]

#[path = "../../src/usb_host/hid_report.rs"]
pub mod hid_report;

pub mod control_protocol;

#[cfg(feature = "nxp-enet")]
pub mod enet_device;

#[cfg(feature = "nxp-device")]
pub mod usb_host {
    pub use crate::hid_report::*;
}

#[cfg(feature = "nxp-device")]
#[path = "../../src/hid_device.rs"]
pub mod hid_device;

#[cfg(feature = "nxp-device")]
pub mod runtime_hid;

#[cfg(feature = "nxp-device")]
#[path = "../../src/macro_engine.rs"]
pub mod macro_engine;

#[cfg(feature = "nxp-device")]
pub mod macro_config {
    include!(concat!(env!("OUT_DIR"), "/macro_config.rs"));
}

/// Marks firmware that relies on the RT1052 reset-time default RAM partition.
///
/// Keeping this call in binaries also retains this crate's native-link
/// metadata when the `nxp-host` feature builds the NXP USB C archive.
#[inline(always)]
pub fn use_nxp_default_flexram() {}

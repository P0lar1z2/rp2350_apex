#![no_std]

//! Shared, platform-independent pieces of the firmware.
//!
//! This crate intentionally contains no Pico SDK or C FFI.  Keeping the USB
//! wire-format code here also lets it be unit-tested on the development PC.

pub mod hid_device;
pub mod usb_host;

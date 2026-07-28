use core::ffi::c_int;

use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

pub const FRAME_CAPACITY: usize = 1536;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EnetStatus {
    pub phy_id1: u16,
    pub phy_id2: u16,
    pub bmsr: u16,
    pub scsr: u16,
    pub link_up: u8,
    pub speed_100m: u8,
    pub full_duplex: u8,
    reserved: u8,
}

unsafe extern "C" {
    fn nxp_enet_init() -> c_int;
    fn nxp_enet_status(status: *mut EnetStatus) -> c_int;
    fn nxp_enet_receive(frame: *mut u8, capacity: u32) -> c_int;
    fn nxp_enet_send(frame: *const u8, length: u32) -> c_int;
    fn nxp_enet_cpu_hz() -> u32;
}

pub struct EnetDevice;

impl EnetDevice {
    pub fn new() -> Result<Self, i32> {
        // SAFETY: The C driver owns one static MAC instance and is initialized once.
        let result = unsafe { nxp_enet_init() };
        if result == 0 { Ok(Self) } else { Err(result) }
    }

    pub fn status(&mut self) -> Result<EnetStatus, i32> {
        let mut status = EnetStatus::default();
        // SAFETY: status is writable and has the same repr(C) layout as the FFI type.
        let result = unsafe { nxp_enet_status(&mut status) };
        if result == 0 { Ok(status) } else { Err(result) }
    }
}

pub fn cpu_hz() -> u32 {
    // SAFETY: The SDK clock query only reads clock-control registers.
    unsafe { nxp_enet_cpu_hz() }
}

pub struct EnetRxToken {
    frame: [u8; FRAME_CAPACITY],
    length: usize,
}

pub struct EnetTxToken;

impl Device for EnetDevice {
    type RxToken<'a> = EnetRxToken;
    type TxToken<'a> = EnetTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut frame = [0u8; FRAME_CAPACITY];
        // SAFETY: frame is writable for FRAME_CAPACITY bytes.
        let length = unsafe { nxp_enet_receive(frame.as_mut_ptr(), FRAME_CAPACITY as u32) };
        (length > 0).then_some((
            EnetRxToken {
                frame,
                length: length as usize,
            },
            EnetTxToken,
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(EnetTxToken)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = 1518;
        capabilities.max_burst_size = Some(1);
        capabilities
    }
}

impl RxToken for EnetRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame[..self.length])
    }
}

impl TxToken for EnetTxToken {
    fn consume<R, F>(self, length: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = [0u8; FRAME_CAPACITY];
        let used = length.min(FRAME_CAPACITY);
        let result = f(&mut frame[..used]);
        if length <= FRAME_CAPACITY {
            // SAFETY: frame contains `length` initialized bytes from the closure.
            let _ = unsafe { nxp_enet_send(frame.as_ptr(), length as u32) };
        }
        result
    }
}

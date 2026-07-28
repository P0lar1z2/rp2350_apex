#![no_std]
#![no_main]

use core::ffi::c_int;
use core::ptr::write_volatile;

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EnetStatus {
    phy_id1: u16,
    phy_id2: u16,
    bmsr: u16,
    scsr: u16,
    link_up: u8,
    speed_100m: u8,
    full_duplex: u8,
    reserved: u8,
}

unsafe extern "C" {
    fn nxp_enet_init() -> c_int;
    fn nxp_enet_status(status: *mut EnetStatus) -> c_int;
    fn nxp_enet_receive(frame: *mut u8, capacity: u32) -> c_int;
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    // SAFETY: VTOR is an aligned Cortex-M system register and this image starts at ITCM 0.
    unsafe { write_volatile(0xE000_ED08 as *mut u32, 0) };
    rtt_init_print!();
    rprintln!("RT1052 Pro ENET DMA probe");

    // SAFETY: The FFI owns its statically allocated ENET state and DMA rings.
    let init = unsafe { nxp_enet_init() };
    rprintln!("ENET DMA init={}", init);
    if init != 0 {
        loop { cortex_m::asm::nop(); }
    }

    let mut frame = [0u8; 1536];
    let mut received = 0u32;
    let mut polls = 0u32;
    loop {
        if polls % 500_000 == 0 {
            let mut status = EnetStatus::default();
            // SAFETY: status points to writable storage matching the C ABI.
            let result = unsafe { nxp_enet_status(&mut status) };
            rprintln!(
                "PHY status={} id={:04x}:{:04x} link={} speed={}M duplex={} rx={}",
                result, status.phy_id1, status.phy_id2, status.link_up,
                if status.speed_100m != 0 { 100 } else { 10 },
                if status.full_duplex != 0 { "full" } else { "half" }, received
            );
        }
        // SAFETY: frame is writable for the supplied capacity; C copies at most capacity bytes.
        let length = unsafe { nxp_enet_receive(frame.as_mut_ptr(), frame.len() as u32) };
        if length > 0 {
            received += 1;
            rprintln!(
                "RX #{} len={} dst={:02x?} src={:02x?} type={:02x}{:02x}",
                received, length, &frame[..6], &frame[6..12], frame[12], frame[13]
            );
        } else if length < 0 {
            rprintln!("RX error={}", length);
        }
        polls = polls.wrapping_add(1);
    }
}

#![no_std]
#![no_main]

use core::ffi::c_int;
use core::ptr::write_volatile;

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};

unsafe extern "C" {
    fn nxp_enet_init() -> c_int;
    fn nxp_enet_send(frame: *const u8, length: u32) -> c_int;
    fn nxp_enet_receive(frame: *mut u8, capacity: u32) -> c_int;
    fn nxp_enet_set_phy_loopback(enable: u8) -> c_int;
    fn nxp_enet_cpu_hz() -> u32;
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    // SAFETY: The RAM image vector table starts at ITCM address zero.
    unsafe { write_volatile(0xE000_ED08 as *mut u32, 0) };
    rtt_init_print!();
    rprintln!("RT1052 <-> LAN8720 RMII local loopback");

    // SAFETY: The C layer owns all ENET state and DMA storage.
    let init = unsafe { nxp_enet_init() };
    // SAFETY: The clock query only reads clock-control registers.
    let core_hz = unsafe { nxp_enet_cpu_hz() };
    rprintln!("ENET init={} core={}Hz", init, core_hz);
    if init != 0 {
        loop {
            cortex_m::asm::nop();
        }
    }
    // SAFETY: ENET is initialized and MDIO address 0 was verified earlier.
    let mode = unsafe { nxp_enet_set_phy_loopback(1) };
    rprintln!("PHY local loopback={}", mode);
    cortex_m::asm::delay(10_000_000);

    let mut received = [0u8; 1536];
    let mut drained = 0u32;
    for _ in 0..16 {
        // SAFETY: received is writable for the declared capacity.
        let length = unsafe { nxp_enet_receive(received.as_mut_ptr(), received.len() as u32) };
        if length <= 0 {
            break;
        }
        drained += 1;
    }
    rprintln!("RX ring drained={}", drained);

    let mut transmitted = [0u8; 60];
    transmitted[..6].fill(0xff);
    transmitted[6..12].copy_from_slice(&[0x02, 0x10, 0x52, 0, 0, 1]);
    transmitted[12..14].copy_from_slice(&[0x88, 0xb5]);
    for (index, byte) in transmitted[14..].iter_mut().enumerate() {
        *byte = (index as u8) ^ 0xa5;
    }
    // SAFETY: transmitted contains 60 readable bytes.
    let send = unsafe { nxp_enet_send(transmitted.as_ptr(), transmitted.len() as u32) };
    rprintln!("TX result={} len={}", send, transmitted.len());

    for _ in 0..20_000_000 {
        // SAFETY: received is writable for the declared capacity.
        let length = unsafe { nxp_enet_receive(received.as_mut_ptr(), received.len() as u32) };
        if length > 0 {
            let length = length as usize;
            let matches =
                length >= transmitted.len() && received[..transmitted.len()] == transmitted;
            rprintln!(
                "RX len={} match={} head={:02x?}",
                length,
                matches,
                &received[..core::cmp::min(length, 20)]
            );
            if matches {
                rprintln!("PASS: RMII local loopback");
            } else {
                rprintln!("FAIL: loopback frame corrupted");
            }
            // SAFETY: Restore auto-negotiation after the destructive diagnostic.
            let _ = unsafe { nxp_enet_set_phy_loopback(0) };
            loop {
                cortex_m::asm::nop();
            }
        }
    }
    rprintln!("FAIL: no loopback frame received");
    // SAFETY: Restore auto-negotiation after timeout.
    let _ = unsafe { nxp_enet_set_phy_loopback(0) };
    loop {
        cortex_m::asm::nop();
    }
}

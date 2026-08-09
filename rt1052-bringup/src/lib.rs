#![no_std]

#[path = "../../src/usb_host/hid_report.rs"]
pub mod hid_report;

pub mod control_protocol;

#[path = "../../src/runtime_trajectory.rs"]
pub mod runtime_trajectory;

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

/// Prepares linker-managed runtime memory before peripheral initialization.
///
/// Flash XIP builds copy the exception/interrupt vector table into ITCM. USB
/// Device builds copy initialized endpoint state from its load address into
/// OCRAM. This must run with interrupts disabled.
#[inline]
pub fn prepare_runtime_memory() {
    #[cfg(feature = "flash-xip")]
    {
        unsafe extern "C" {
            static __vector_table: u32;
        }

        const VECTOR_BYTES: usize = 0x400;
        let source = core::ptr::addr_of!(__vector_table);
        // SAFETY: The RT1052 default FlexRAM partition maps 128 KiB of ITCM at
        // zero. The linked table is readable from FlexSPI and is 1 KiB or less.
        unsafe {
            for index in 0..VECTOR_BYTES / 4 {
                let value = core::ptr::read_volatile(source.add(index));
                write_itcm_word(index * 4, value);
            }
        }
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
    }

    #[cfg(feature = "nxp-device")]
    {
        unsafe extern "C" {
            static __usb_device_load: u8;
            static mut __usb_device_start: u8;
            static mut __usb_device_end: u8;
        }
        let source = core::ptr::addr_of!(__usb_device_load);
        let destination = core::ptr::addr_of_mut!(__usb_device_start);
        let end = core::ptr::addr_of_mut!(__usb_device_end);
        let length = end as usize - destination as usize;
        if source as usize != destination as usize {
            // SAFETY: The linker reserves non-overlapping load and runtime
            // regions for the initialized USB device state.
            unsafe { core::ptr::copy_nonoverlapping(source, destination, length) };
        }
        cortex_m::asm::dsb();
    }
}

#[cfg(feature = "flash-xip")]
#[inline(always)]
unsafe fn write_itcm_word(address: usize, value: u32) {
    // Keep the address dynamic: zero is valid mapped ITCM on Cortex-M, though
    // it is the null address in Rust's abstract machine.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

/// Marks firmware that relies on the RT1052 reset-time default RAM partition.
///
/// Keeping this call in binaries also retains this crate's native-link
/// metadata when the `nxp-host` feature builds the NXP USB C archive.
#[inline(always)]
pub fn use_nxp_default_flexram() {}

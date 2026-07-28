#![no_std]
#![no_main]

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};

const IOMUXC_GPR_GPR14: usize = 0x400A_C038;
const IOMUXC_GPR_GPR16: usize = 0x400A_C040;
const IOMUXC_GPR_GPR17: usize = 0x400A_C044;
const SCB_VTOR: usize = 0xE000_ED08;

#[repr(C, align(32))]
struct DmaSmoke([u32; 8]);

#[unsafe(link_section = ".usb_dma.smoke")]
static mut DMA_SMOKE: DmaSmoke = DmaSmoke([0; 8]);

#[inline]
fn read32(address: usize) -> u32 {
    // SAFETY: The caller supplies an aligned RT1052 MMIO or mapped RAM address.
    unsafe { read_volatile(address as *const u32) }
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    // SAFETY: VTOR is an aligned, writable Cortex-M7 system register.
    unsafe { write_volatile(SCB_VTOR as *mut u32, 0) };
    rtt_init_print!();

    let gpr14 = read32(IOMUXC_GPR_GPR14);
    let gpr16 = read32(IOMUXC_GPR_GPR16);
    let gpr17 = read32(IOMUXC_GPR_GPR17);

    rprintln!("RT1052 USB host probe: stage 0 / FlexRAM");
    rprintln!(
        "GPR14={:#010x} GPR16={:#010x} GPR17={:#010x}",
        gpr14,
        gpr16,
        gpr17
    );

    // SAFETY: The probe owns DMA_SMOKE for its entire lifetime and only uses
    // volatile raw-pointer accesses below.
    let dma = unsafe { addr_of_mut!(DMA_SMOKE.0).cast::<u32>() };
    let expected = [
        0x1052_0000,
        0xA55A_5AA5,
        0x0123_4567,
        0x89AB_CDEF,
        0x0000_0001,
        0xFFFF_FFFE,
        0x1357_9BDF,
        0x2468_ACE0,
    ];

    for (index, value) in expected.iter().copied().enumerate() {
        // SAFETY: DMA_SMOKE owns eight aligned u32 words in the OCRAM section.
        unsafe { write_volatile(dma.add(index), value) };
    }

    let mut ok = true;
    for (index, value) in expected.iter().copied().enumerate() {
        // SAFETY: Same bounds and alignment argument as the write loop.
        let actual = unsafe { read_volatile(dma.add(index)) };
        ok &= actual == value;
    }

    if ok {
        rprintln!(
            "PASS: OCRAM DMA window at {:#010x}, 32-byte aligned",
            dma as usize
        );
        rprintln!("NEXT: initialize USBPHY2 + EHCI host");
    } else {
        rprintln!(
            "ERROR: OCRAM write/read verification failed at {:#010x}",
            dma as usize
        );
    }

    loop {
        // Keep the core/debug clock running so a five-wire CMSIS-DAP probe can
        // halt and inspect the result. WFI may gate the RT1052 debug path.
        cortex_m::asm::nop();
    }
}

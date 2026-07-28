#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_halt as _;

// Register addresses verified against the NXP MIMXRT1052 SDK bundled with the
// EmbedFire board documentation.
const CCM_CCGR1: usize = 0x400F_C06C;
const SCB_VTOR: usize = 0xE000_ED08;
const IOMUXC_MUX_GPIO_AD_B0_09: usize = 0x401F_80E0;
const IOMUXC_PAD_GPIO_AD_B0_09: usize = 0x401F_82D0;
const IOMUXC_MUX_GPIO_AD_B0_10: usize = 0x401F_80E4;
const IOMUXC_PAD_GPIO_AD_B0_10: usize = 0x401F_82D4;
const IOMUXC_MUX_GPIO_AD_B1_08: usize = 0x401F_811C;
const IOMUXC_PAD_GPIO_AD_B1_08: usize = 0x401F_830C;
const IOMUXC_MUX_GPIO_AD_B1_09: usize = 0x401F_8120;
const IOMUXC_PAD_GPIO_AD_B1_09: usize = 0x401F_8310;
const GPIO1_DR: usize = 0x401B_8000;
const GPIO1_GDIR: usize = 0x401B_8004;

const GPIO1_CLOCK_GATE_MASK: u32 = 0b11 << 26;
const LED_CORE: u32 = 1 << 9;
const LED_BLUE: u32 = 1 << 10;
const LED_RED: u32 = 1 << 24;
const LED_GREEN: u32 = 1 << 25;
const ALL_LEDS: u32 = LED_CORE | LED_RED | LED_GREEN | LED_BLUE;

#[inline]
fn read32(address: usize) -> u32 {
    // SAFETY: Each caller supplies an aligned, valid RT1052 MMIO address.
    unsafe { read_volatile(address as *const u32) }
}

#[inline]
fn write32(address: usize, value: u32) {
    // SAFETY: Each caller supplies an aligned, valid RT1052 MMIO address.
    unsafe { write_volatile(address as *mut u32, value) }
}

#[entry]
fn main() -> ! {
    // We enter from a debugger rather than a hardware reset, so discard any
    // interrupt state left by the previously running image and point the
    // vector table at this ITCM image.
    cortex_m::interrupt::disable();
    write32(SCB_VTOR, 0x0000_0000);

    // Enable GPIO1 in all clock modes.
    write32(CCM_CCGR1, read32(CCM_CCGR1) | GPIO1_CLOCK_GATE_MASK);

    // Configure the core-board LED and every channel of the Pro board's RGB
    // LED. All four outputs are active-low.
    write32(IOMUXC_MUX_GPIO_AD_B0_09, 5);
    write32(IOMUXC_PAD_GPIO_AD_B0_09, 0x0000_00B0);
    write32(IOMUXC_MUX_GPIO_AD_B0_10, 5);
    write32(IOMUXC_PAD_GPIO_AD_B0_10, 0x0000_00B0);
    write32(IOMUXC_MUX_GPIO_AD_B1_08, 5);
    write32(IOMUXC_PAD_GPIO_AD_B1_08, 0x0000_00B0);
    write32(IOMUXC_MUX_GPIO_AD_B1_09, 5);
    write32(IOMUXC_PAD_GPIO_AD_B1_09, 0x0000_00B0);

    write32(GPIO1_GDIR, read32(GPIO1_GDIR) | ALL_LEDS);

    // Blink the controllable core-board LED and all three channels of the Pro
    // RGB LED together. The separate red power indicators are not GPIO driven
    // and therefore remain steadily lit.
    loop {
        write32(GPIO1_DR, read32(GPIO1_DR) & !ALL_LEDS);
        cortex_m::asm::delay(70_000_000);
        write32(GPIO1_DR, read32(GPIO1_DR) | ALL_LEDS);
        cortex_m::asm::delay(70_000_000);
    }
}

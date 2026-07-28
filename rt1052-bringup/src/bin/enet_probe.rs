#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};

const SCB_VTOR: usize = 0xE000_ED08;

const CCM_CCGR1: usize = 0x400F_C06C;
const CCM_ANALOG_PLL_ENET: usize = 0x400D_80E0;
const IOMUXC_GPR_GPR1: usize = 0x400A_C004;

const GPIO1_DR: usize = 0x401B_8000;
const GPIO1_GDIR: usize = 0x401B_8004;
const PHY_RESET: u32 = 1 << 9;
const PHY_INT_STRAP: u32 = 1 << 10;

const MUX_GPIO_AD_B0_09: usize = 0x401F_80E0;
const PAD_GPIO_AD_B0_09: usize = 0x401F_82D0;
const MUX_GPIO_AD_B0_10: usize = 0x401F_80E4;
const PAD_GPIO_AD_B0_10: usize = 0x401F_82D4;
const MUX_GPIO_AD_B1_04: usize = 0x401F_810C;
const PAD_GPIO_AD_B1_04: usize = 0x401F_82FC;
const MUX_GPIO_B1_10: usize = 0x401F_81A4;
const PAD_GPIO_B1_10: usize = 0x401F_8394;
const SELECT_ENET_REF_CLK: usize = 0x401F_842C;
const MUX_GPIO_B1_15: usize = 0x401F_81B8;
const PAD_GPIO_B1_15: usize = 0x401F_83A8;
const SELECT_ENET_MDIO: usize = 0x401F_8430;

const ENET_BASE: usize = 0x402D_8000;
const ENET_EIR: usize = ENET_BASE + 0x04;
const ENET_MMFR: usize = ENET_BASE + 0x40;
const ENET_MSCR: usize = ENET_BASE + 0x44;
const ENET_EIR_MII: u32 = 1 << 23;

const PHY_ADDRESS: u8 = 0;
const PHY_BMCR: u8 = 0;
const PHY_BMSR: u8 = 1;
const PHY_ID1: u8 = 2;
const PHY_ID2: u8 = 3;
const PHY_SPECIAL_CONTROL_STATUS: u8 = 31;

#[inline]
fn read32(address: usize) -> u32 {
    // SAFETY: All addresses passed here are aligned RT1052 MMIO registers.
    unsafe { read_volatile(address as *const u32) }
}

#[inline]
fn write32(address: usize, value: u32) {
    // SAFETY: All addresses passed here are aligned RT1052 MMIO registers.
    unsafe { write_volatile(address as *mut u32, value) }
}

fn init_enet_pll_50mhz() -> bool {
    const DIV_SELECT_50MHZ: u32 = 1;
    const POWERDOWN: u32 = 1 << 12;
    const ENABLE: u32 = 1 << 13;
    const BYPASS: u32 = 1 << 16;
    const LOCK: u32 = 1 << 31;

    let original = read32(CCM_ANALOG_PLL_ENET);
    write32(CCM_ANALOG_PLL_ENET, original | BYPASS);
    let configured = (read32(CCM_ANALOG_PLL_ENET) & !(0b11 | POWERDOWN))
        | DIV_SELECT_50MHZ
        | ENABLE;
    write32(CCM_ANALOG_PLL_ENET, configured);

    for _ in 0..2_000_000 {
        if read32(CCM_ANALOG_PLL_ENET) & LOCK != 0 {
            write32(CCM_ANALOG_PLL_ENET, read32(CCM_ANALOG_PLL_ENET) & !BYPASS);
            return true;
        }
    }
    false
}

fn init_board_phy() -> bool {
    // Enable GPIO1 and ENET clocks in every power mode.
    write32(CCM_CCGR1, read32(CCM_CCGR1) | (0b11 << 26) | (0b11 << 10));

    // LAN8720 nRST and nINT/REFCLKO strap pins from the Pro board schematic/example.
    write32(MUX_GPIO_AD_B0_09, 5);
    write32(PAD_GPIO_AD_B0_09, 0x0000_B0A9);
    write32(MUX_GPIO_AD_B0_10, 5);
    write32(PAD_GPIO_AD_B0_10, 0x0000_B0A9);
    write32(GPIO1_GDIR, read32(GPIO1_GDIR) | PHY_RESET | PHY_INT_STRAP);
    write32(GPIO1_DR, read32(GPIO1_DR) | PHY_INT_STRAP);
    write32(GPIO1_DR, read32(GPIO1_DR) & !PHY_RESET);

    // MDC, MDIO and the 50 MHz RMII reference clock output.
    write32(MUX_GPIO_AD_B1_04, 1);
    write32(PAD_GPIO_AD_B1_04, 0x0000_B0E9);
    write32(MUX_GPIO_B1_15, 0);
    write32(SELECT_ENET_MDIO, 2);
    write32(PAD_GPIO_B1_15, 0x0000_B0E9);
    write32(MUX_GPIO_B1_10, 6);
    write32(SELECT_ENET_REF_CLK, 1);
    write32(PAD_GPIO_B1_10, 0x31);
    write32(IOMUXC_GPR_GPR1, read32(IOMUXC_GPR_GPR1) | (1 << 17));

    let pll_locked = init_enet_pll_50mhz();
    cortex_m::asm::delay(5_000_000);
    write32(GPIO1_DR, read32(GPIO1_DR) | PHY_RESET);
    cortex_m::asm::delay(10_000_000);
    pll_locked
}

fn mdio_read(register: u8) -> Option<u16> {
    // A conservative divisor keeps MDC below 2.5 MHz for every supported IPG clock.
    // MII_SPEED=63 gives IPG/(2*(63+1)); HOLDTIME=2 satisfies the PHY hold time.
    write32(ENET_MSCR, (63 << 1) | (2 << 8));
    write32(ENET_EIR, ENET_EIR_MII);
    let frame = (1 << 30)
        | (2 << 28)
        | ((PHY_ADDRESS as u32) << 23)
        | ((register as u32) << 18)
        | (2 << 16);
    write32(ENET_MMFR, frame);

    for _ in 0..2_000_000 {
        if read32(ENET_EIR) & ENET_EIR_MII != 0 {
            let data = read32(ENET_MMFR) as u16;
            write32(ENET_EIR, ENET_EIR_MII);
            return Some(data);
        }
    }
    None
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    write32(SCB_VTOR, 0);
    rtt_init_print!();

    rprintln!("RT1052 Pro Ethernet PHY probe");
    let pll_locked = init_board_phy();
    rprintln!(
        "ENET PLL: {} ({:#010x})",
        if pll_locked { "locked" } else { "LOCK TIMEOUT" },
        read32(CCM_ANALOG_PLL_ENET)
    );

    let id1 = mdio_read(PHY_ID1);
    let id2 = mdio_read(PHY_ID2);
    let bmcr = mdio_read(PHY_BMCR);
    // BMSR link is latch-low, so the second read is the current state.
    let _ = mdio_read(PHY_BMSR);
    let bmsr = mdio_read(PHY_BMSR);
    let special = mdio_read(PHY_SPECIAL_CONTROL_STATUS);

    rprintln!(
        "PHY addr={} ID1={:?} ID2={:?} BMCR={:?} BMSR={:?} SCSR={:?}",
        PHY_ADDRESS,
        id1,
        id2,
        bmcr,
        bmsr,
        special
    );
    match (id1, id2) {
        (Some(0x0007), Some(id2)) if id2 & 0xFFF0 == 0xC0F0 => {
            let status = special.unwrap_or(0);
            let mode = (status >> 2) & 0x7;
            rprintln!(
                "PASS: LAN8720 family detected; link={} negotiated_mode={}",
                bmsr.is_some_and(|value| value & 0x0004 != 0),
                mode
            );
        }
        (Some(0xFFFF), Some(0xFFFF)) | (Some(0), Some(0)) => {
            rprintln!("ERROR: no PHY response on MDIO address 0");
        }
        _ => rprintln!("ERROR: unexpected PHY identity or MDIO timeout"),
    }

    loop {
        cortex_m::asm::nop();
    }
}

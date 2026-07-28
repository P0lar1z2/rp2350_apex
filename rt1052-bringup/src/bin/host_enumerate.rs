#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};

const IOMUXC_GPR_GPR14: usize = 0x400A_C038;
const IOMUXC_GPR_GPR16: usize = 0x400A_C040;
const IOMUXC_GPR_GPR17: usize = 0x400A_C044;
const SCB_VTOR: usize = 0xE000_ED08;
const NVIC_ISER3: usize = 0xE000_E10C;
const NVIC_ICPR3: usize = 0xE000_E28C;
const NVIC_IPR_USB_OTG2: usize = 0xE000_E470;
const USB_OTG2_VECTOR: usize = (16 + 112) * 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct HostEvent {
    kind: u8,
    status: u8,
    speed: u8,
    address: u8,
    hub_address: u8,
    hub_port: u8,
    endpoint_address: u8,
    interval: u8,
    vid: u16,
    pid: u16,
    max_packet_size: u16,
    interface_number: u8,
    interface_protocol: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HostReport {
    sequence: u32,
    length: u8,
    status: u8,
    data: [u8; 16],
    reserved: u16,
}

unsafe extern "C" {
    fn nxp_host_init() -> i32;
    fn nxp_host_task();
    fn nxp_host_irq();
    fn nxp_host_pop_event(event: *mut HostEvent) -> i32;
    fn nxp_host_pop_report(report: *mut HostReport) -> i32;
}

#[inline]
fn read32(address: usize) -> u32 {
    // SAFETY: Callers provide valid, aligned RT1052 register addresses.
    unsafe { read_volatile(address as *const u32) }
}

#[inline]
fn write32(address: usize, value: u32) {
    // SAFETY: Callers provide valid, aligned RT1052 register/vector addresses.
    unsafe { write_volatile(address as *mut u32, value) }
}

unsafe extern "C" fn usb_otg2_irq() {
    // SAFETY: The C shim guards its host handle and only touches USB2 state.
    unsafe { nxp_host_irq() };
}

fn speed_name(speed: u8) -> &'static str {
    match speed {
        0 => "full-speed (1K max interrupt polling)",
        1 => "low-speed",
        2 => "high-speed",
        _ => "unknown",
    }
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    write32(SCB_VTOR, 0);
    rtt_init_print!();

    let gpr14 = read32(IOMUXC_GPR_GPR14);
    let gpr16 = read32(IOMUXC_GPR_GPR16);
    let gpr17 = read32(IOMUXC_GPR_GPR17);
    rprintln!(
        "FlexRAM reset-halt state: GPR14={:#x} GPR16={:#x} GPR17={:#x}",
        gpr14,
        gpr16,
        gpr17
    );

    if let Some(mut peripherals) = cortex_m::Peripherals::take() {
        peripherals.SCB.disable_dcache(&mut peripherals.CPUID);
    }

    // cortex-m-rt's generic vector table contains DefaultHandler entries. ITCM
    // is writable, so install the RT1052 USB_OTG2 handler before unmasking it.
    write32(USB_OTG2_VECTOR, usb_otg2_irq as *const () as usize as u32);

    // SAFETY: The reset-time default OCRAM and D-cache state are prepared
    // before NXP allocates EHCI data.
    let status = unsafe { nxp_host_init() };
    rprintln!("NXP USB Host 2.12.2 init status={}", status);
    if status != 0 {
        loop {
            cortex_m::asm::bkpt();
        }
    }

    // IRQ 112: clear pending, priority 3 of 16, then enable.
    write32(NVIC_ICPR3, 1 << 16);
    unsafe { write_volatile(NVIC_IPR_USB_OTG2 as *mut u8, 3 << 4) };
    write32(NVIC_ISER3, 1 << 16);
    unsafe { cortex_m::interrupt::enable() };
    rprintln!("OTG2 EHCI running; waiting for FE1.1S hub and HID mouse");

    loop {
        // SAFETY: Bare-metal task function is called from this one main loop.
        unsafe { nxp_host_task() };

        let mut event = HostEvent {
            kind: 0,
            status: 0,
            speed: 0,
            address: 0,
            hub_address: 0,
            hub_port: 0,
            endpoint_address: 0,
            interval: 0,
            vid: 0,
            pid: 0,
            max_packet_size: 0,
            interface_number: 0,
            interface_protocol: 0,
        };
        // SAFETY: `event` is valid writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => {
                    rprintln!(
                        "HID {:04x}:{:04x} addr={} via hub={} port={} interface={} protocol={}",
                        event.vid,
                        event.pid,
                        event.address,
                        event.hub_address,
                        event.hub_port,
                        event.interface_number,
                        event.interface_protocol
                    );
                    rprintln!(
                        "speed={} ep={:#04x} wMaxPacketSize={} bInterval={}",
                        speed_name(event.speed),
                        event.endpoint_address,
                        event.max_packet_size,
                        event.interval
                    );
                    if event.speed == 2 && event.interval == 1 {
                        rprintln!("8K-capable source confirmed; next stage is 8 prequeued qTDs");
                    } else {
                        rprintln!("source is not HS bInterval=1; do not claim 8K");
                    }
                }
                2 => rprintln!("HID detached"),
                3 => rprintln!("enumeration failed, status={}", event.status),
                4 if event.status == 0 => rprintln!("HID Interrupt IN receiver ready"),
                4 => rprintln!("HID receiver setup failed, status={}", event.status),
                _ => {}
            }
        }

        let mut report = HostReport {
            sequence: 0,
            length: 0,
            status: 0,
            data: [0; 16],
            reserved: 0,
        };
        // SAFETY: `report` is valid writable storage matching the C ABI.
        while unsafe { nxp_host_pop_report(&mut report) } != 0 {
            let length = usize::from(report.length.min(16));
            if report.sequence <= 16 || (report.sequence & 127) == 0 || report.status != 0 {
                rprintln!(
                    "report #{} status={} len={} data={:02x?}",
                    report.sequence,
                    report.status,
                    length,
                    &report.data[..length]
                );
            }
        }
    }
}

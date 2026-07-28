#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rt1052_bringup::hid_report::{DecodedReport, ReportDecoder, parse_report_descriptor};
use rtt_target::{ChannelMode::NoBlockSkip, rprintln, rtt_init_print};

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
    interface_subclass: u8,
    interface_protocol: u8,
    reserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HostReport {
    sequence: u32,
    length: u8,
    status: u8,
    data: [u8; 64],
    reserved: u16,
}

const _: () = assert!(core::mem::size_of::<HostEvent>() == 18);
const _: () = assert!(core::mem::size_of::<HostReport>() == 72);

unsafe extern "C" {
    fn nxp_host_init() -> i32;
    fn nxp_host_task();
    fn nxp_host_irq();
    fn nxp_host_pop_event(event: *mut HostEvent) -> i32;
    fn nxp_host_pop_report(report: *mut HostReport) -> i32;
    fn nxp_host_copy_report_descriptor(buffer: *mut u8, capacity: u16) -> i32;
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
    rtt_init_print!(NoBlockSkip, 4096);

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

    let mut decoder = ReportDecoder::empty();

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
            interface_subclass: 0,
            interface_protocol: 0,
            reserved: 0,
        };
        // SAFETY: `event` is valid writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => {
                    rprintln!(
                        "HID {:04x}:{:04x} addr={} via hub={} port={} interface={} subclass={} protocol={}",
                        event.vid,
                        event.pid,
                        event.address,
                        event.hub_address,
                        event.hub_port,
                        event.interface_number,
                        event.interface_subclass,
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
                5 if event.status == 0 => {
                    let mut descriptor = [0u8; 512];
                    // SAFETY: The destination has the advertised 512-byte capacity.
                    let length =
                        unsafe { nxp_host_copy_report_descriptor(descriptor.as_mut_ptr(), 512) };
                    if length > 0 {
                        let length = usize::try_from(length).unwrap_or(0).min(descriptor.len());
                        rprintln!("HID Report Descriptor: {} bytes", length);
                        for (offset, chunk) in descriptor[..length].chunks(16).enumerate() {
                            rprintln!("  {:03x}: {:02x?}", offset * 16, chunk);
                        }
                        decoder = parse_report_descriptor(&descriptor[..length]);
                        if decoder.is_empty() {
                            rprintln!("Rust HID parser found no supported input layout");
                        } else {
                            rprintln!("Rust HID parser ready");
                        }
                    } else {
                        rprintln!("HID Report Descriptor callback returned no data");
                    }
                }
                5 => rprintln!(
                    "HID Report Descriptor request failed, status={}",
                    event.status
                ),
                _ => {}
            }
        }

        let mut report = HostReport {
            sequence: 0,
            length: 0,
            status: 0,
            data: [0; 64],
            reserved: 0,
        };
        // SAFETY: `report` is valid writable storage matching the C ABI.
        while unsafe { nxp_host_pop_report(&mut report) } != 0 {
            let length = usize::from(report.length.min(64));
            if report.sequence <= 16 || (report.sequence & 127) == 0 || report.status != 0 {
                rprintln!(
                    "report #{} status={} len={} data={:02x?}",
                    report.sequence,
                    report.status,
                    length,
                    &report.data[..length]
                );
                match decoder.decode(&report.data[..length]) {
                    Some(DecodedReport::Mouse(mouse)) => rprintln!(
                        "mouse buttons={:#04x} x={} y={} wheel={} pan={}",
                        mouse.buttons,
                        mouse.x,
                        mouse.y,
                        mouse.wheel,
                        mouse.pan
                    ),
                    Some(DecodedReport::Keyboard(keyboard)) => rprintln!(
                        "keyboard modifiers={:#04x} keys={:02x?}",
                        keyboard.modifiers,
                        keyboard.keys
                    ),
                    Some(DecodedReport::Consumer(usage)) => {
                        rprintln!("consumer usage={:#06x}", usage)
                    }
                    None => rprintln!("report did not match a supported HID layout"),
                }
            }
        }
    }
}

#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use imxrt_ral as ral;
use imxrt_usbd::{BusAdapter, EndpointMemory, EndpointState, Instances};
use panic_rtt_target as _;
use rt1052_bringup::{
    hid_report::{DecodedReport, parse_report_descriptor},
    runtime_hid::{RuntimeHid, high_speed_interval},
};
use rtt_target::{ChannelMode::NoBlockSkip, rprintln, rtt_init_print};
use usb_device::{
    UsbError,
    bus::UsbBusAllocator,
    device::{StringDescriptors, UsbDeviceBuilder, UsbDeviceState, UsbVidPid},
};

const SCB_VTOR: usize = 0xE000_ED08;
const NVIC_ISER3: usize = 0xE000_E10C;
const NVIC_ICPR3: usize = 0xE000_E28C;
const NVIC_IPR_USB_OTG2: usize = 0xE000_E470;
const USB_OTG2_VECTOR: usize = (16 + 112) * 4;

#[unsafe(link_section = ".usb_device.endpoint_memory")]
static EP_MEMORY: EndpointMemory<2048> = EndpointMemory::new();

#[unsafe(link_section = ".usb_device.endpoint_state")]
static EP_STATE: EndpointState = EndpointState::max_endpoints();

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

impl HostEvent {
    const fn empty() -> Self {
        Self {
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
        }
    }
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

impl HostReport {
    const fn empty() -> Self {
        Self {
            sequence: 0,
            length: 0,
            status: 0,
            data: [0; 64],
            reserved: 0,
        }
    }
}

unsafe extern "C" {
    fn nxp_host_init() -> i32;
    fn nxp_host_task();
    fn nxp_host_irq();
    fn nxp_host_pop_event(event: *mut HostEvent) -> i32;
    fn nxp_host_pop_report(report: *mut HostReport) -> i32;
    fn nxp_host_copy_report_descriptor(buffer: *mut u8, capacity: u16) -> i32;
    fn nxp_device_init_clocks() -> i32;
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

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    write32(SCB_VTOR, 0);
    rtt_init_print!(NoBlockSkip, 4096);

    if let Some(mut peripherals) = cortex_m::Peripherals::take() {
        peripherals.SCB.disable_dcache(&mut peripherals.CPUID);
    }

    write32(USB_OTG2_VECTOR, usb_otg2_irq as *const () as usize as u32);
    // SAFETY: USB2 is exclusively owned by the NXP Host stack.
    let host_status = unsafe { nxp_host_init() };
    rprintln!("OTG2 NXP Host init status={}", host_status);
    if host_status != 0 {
        loop {
            cortex_m::asm::bkpt();
        }
    }

    write32(NVIC_ICPR3, 1 << 16);
    // SAFETY: USB_OTG2 is IRQ 112, configured at priority 3 of 16.
    unsafe { write_volatile(NVIC_IPR_USB_OTG2 as *mut u8, 3 << 4) };
    write32(NVIC_ISER3, 1 << 16);
    // SAFETY: The USB2 vector and peripheral state are initialized above.
    unsafe { cortex_m::interrupt::enable() };

    rprintln!("waiting for OTG2 source profile before attaching OTG1");
    let mut source = HostEvent::empty();
    let mut have_source = false;
    let mut report_descriptor = [0u8; 512];
    let report_descriptor_len = 'profile: loop {
        // SAFETY: Called only from this main loop; USB2 IRQ handles controller events.
        unsafe { nxp_host_task() };
        let mut event = HostEvent::empty();
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => {
                    source = event;
                    have_source = true;
                    rprintln!(
                        "source {:04x}:{:04x} speed={} subclass={} protocol={} packet={} interval={}",
                        source.vid,
                        source.pid,
                        source.speed,
                        source.interface_subclass,
                        source.interface_protocol,
                        source.max_packet_size,
                        source.interval
                    );
                }
                2 => {
                    have_source = false;
                    rprintln!("source detached while reading profile");
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => rprintln!("OTG2 Interrupt IN ready"),
                4 => rprintln!("OTG2 receiver failed status={}", event.status),
                5 if event.status == 0 && have_source => {
                    // SAFETY: Destination capacity matches the FFI argument.
                    let length = unsafe {
                        nxp_host_copy_report_descriptor(report_descriptor.as_mut_ptr(), 512)
                    };
                    if length > 0 {
                        let length = usize::try_from(length)
                            .unwrap_or(0)
                            .min(report_descriptor.len());
                        break 'profile length;
                    }
                }
                5 => rprintln!("OTG2 descriptor failed status={}", event.status),
                _ => {}
            }
        }
    };

    let upstream_interval = high_speed_interval(source.speed, source.interval);
    rprintln!(
        "profile ready: descriptor={} bytes, HS interval={} (source interval={})",
        report_descriptor_len,
        upstream_interval,
        source.interval
    );
    if source.max_packet_size > 64 {
        rprintln!(
            "unsupported source packet size {}; bridge capacity is 64 bytes",
            source.max_packet_size
        );
        loop {
            // Keep servicing OTG2 so detach and controller state remain clean.
            unsafe { nxp_host_task() };
        }
    }

    // SAFETY: USB1 is exclusively owned by the Rust Device stack.
    let device_clock_status = unsafe { nxp_device_init_clocks() };
    rprintln!("OTG1 Device clock status={}", device_clock_status);
    if device_clock_status != 0 {
        loop {
            cortex_m::asm::bkpt();
        }
    }

    let instances = Instances {
        // SAFETY: Each USB1 singleton is fabricated exactly once.
        usb: unsafe { ral::usb::USB1::instance() },
        usbnc: unsafe { ral::usbnc::USBNC1::instance() },
        usbphy: unsafe { ral::usbphy::USBPHY1::instance() },
    };
    let bus = UsbBusAllocator::new(BusAdapter::new(instances, &EP_MEMORY, &EP_STATE));
    let mut hid = RuntimeHid::new(
        &bus,
        &report_descriptor[..report_descriptor_len],
        source.max_packet_size.clamp(1, 64),
        upstream_interval,
        source.interface_subclass,
        source.interface_protocol,
    );
    let strings = [StringDescriptors::default()
        .manufacturer("xense")
        .product("RT1052 Dynamic HID Clone")
        .serial_number("RAM-CLONE")];
    let mut device = UsbDeviceBuilder::new(&bus, UsbVidPid(source.vid, source.pid))
        .strings(&strings)
        .expect("valid static USB strings")
        .device_class(0)
        .max_packet_size_0(64)
        .expect("64-byte EP0 is valid for high-speed USB")
        .build();

    rprintln!("dynamic clone running: OTG2 raw HID -> OTG1 cloned HID");
    let decoder = parse_report_descriptor(&report_descriptor[..report_descriptor_len]);
    let mut device_configured = false;
    let mut forwarded = 0u32;
    let mut dropped = 0u32;

    loop {
        // SAFETY: Called only from this main loop; USB2 IRQ handles controller events.
        unsafe { nxp_host_task() };
        let _ = device.poll(&mut [&mut hid]);
        if device.state() == UsbDeviceState::Configured {
            if !device_configured {
                device.bus().configure();
                device_configured = true;
                rprintln!("OTG1 bridge configured by PC");
            }
        } else {
            device_configured = false;
        }

        let mut event = HostEvent::empty();
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => rprintln!(
                    "OTG2 HID {:04x}:{:04x} speed={} ep={:#04x} interval={}",
                    event.vid,
                    event.pid,
                    event.speed,
                    event.endpoint_address,
                    event.interval
                ),
                2 => {
                    rprintln!("OTG2 HID detached; reconnect requires OTG1 re-enumeration");
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => rprintln!("OTG2 Interrupt IN ready"),
                4 => rprintln!("OTG2 receiver failed status={}", event.status),
                5 if event.status == 0 => {
                    rprintln!("OTG2 descriptor refreshed; active clone remains unchanged");
                }
                5 => rprintln!("OTG2 descriptor failed status={}", event.status),
                _ => {}
            }
        }

        let mut report = HostReport::empty();
        // SAFETY: `report` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_report(&mut report) } != 0 {
            let length = usize::from(report.length.min(64));
            if !device_configured {
                dropped = dropped.wrapping_add(1);
                continue;
            }
            match hid.push_report(&report.data[..length]) {
                Ok(_) => forwarded = forwarded.wrapping_add(1),
                Err(UsbError::WouldBlock) => dropped = dropped.wrapping_add(1),
                Err(_) => dropped = dropped.wrapping_add(1),
            }
            if forwarded <= 16 || forwarded & 127 == 0 {
                match decoder.decode(&report.data[..length]) {
                    Some(DecodedReport::Mouse(mouse)) => rprintln!(
                        "clone #{} buttons={:#04x} x={} y={} wheel={} pan={} dropped={}",
                        forwarded,
                        mouse.buttons,
                        mouse.x,
                        mouse.y,
                        mouse.wheel,
                        mouse.pan,
                        dropped
                    ),
                    _ => rprintln!(
                        "clone #{} raw_len={} dropped={}",
                        forwarded,
                        length,
                        dropped
                    ),
                }
            }
        }

        // Preserve an observable MMIO read in this polling loop.
        unsafe { read_volatile(SCB_VTOR as *const u32) };
    }
}

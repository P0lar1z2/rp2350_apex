#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use imxrt_ral as ral;
use imxrt_usbd::{BusAdapter, EndpointMemory, EndpointState, Instances};
use panic_rtt_target as _;
use rt1052_bringup::hid_device::KeyboardMouseHid;
use rtt_target::{ChannelMode::NoBlockSkip, rprintln, rtt_init_print};
use usb_device::{
    bus::UsbBusAllocator,
    device::{StringDescriptors, UsbDeviceBuilder, UsbDeviceState, UsbVidPid},
};

const SCB_VTOR: usize = 0xE000_ED08;
const USB1_USBCMD: usize = 0x402E_0140;
const USB1_USBSTS: usize = 0x402E_0144;
const USB1_PORTSC1: usize = 0x402E_0184;
const USB1_OTGSC: usize = 0x402E_01A4;
const USB1_USBMODE: usize = 0x402E_01A8;

#[unsafe(link_section = ".usb_device.endpoint_memory")]
static EP_MEMORY: EndpointMemory<2048> = EndpointMemory::new();

#[unsafe(link_section = ".usb_device.endpoint_state")]
static EP_STATE: EndpointState = EndpointState::max_endpoints();

unsafe extern "C" {
    fn nxp_device_init_clocks() -> i32;
}

#[inline]
fn read32(address: usize) -> u32 {
    // SAFETY: Callers provide valid, aligned RT1052 register addresses.
    unsafe { read_volatile(address as *const u32) }
}

#[inline]
fn write32(address: usize, value: u32) {
    // SAFETY: Callers provide valid, aligned RT1052 register addresses.
    unsafe { write_volatile(address as *mut u32, value) }
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    write32(SCB_VTOR, 0);
    rtt_init_print!(NoBlockSkip, 2048);

    if let Some(mut peripherals) = cortex_m::Peripherals::take() {
        peripherals.SCB.disable_dcache(&mut peripherals.CPUID);
    }

    // SAFETY: This binary exclusively owns USB1 / USBPHY1, while the NXP
    // Host stack is not initialized and therefore does not touch USB2 either.
    let clock_status = unsafe { nxp_device_init_clocks() };
    rprintln!("OTG1 clock init status={}", clock_status);
    if clock_status != 0 {
        loop {
            cortex_m::asm::bkpt();
        }
    }

    let instances = Instances {
        // SAFETY: Each singleton is fabricated once and is owned by BusAdapter.
        usb: unsafe { ral::usb::USB1::instance() },
        usbnc: unsafe { ral::usbnc::USBNC1::instance() },
        usbphy: unsafe { ral::usbphy::USBPHY1::instance() },
    };
    let bus = UsbBusAllocator::new(BusAdapter::new(instances, &EP_MEMORY, &EP_STATE));
    rprintln!("OTG1 BusAdapter initialized");
    let mut hid = KeyboardMouseHid::new(&bus);
    let strings = [StringDescriptors::default()
        .manufacturer("xense")
        .product("RT1052 Rust HID")
        .serial_number("RAM-PROBE")];
    let mut device = UsbDeviceBuilder::new(&bus, UsbVidPid(0x1209, 0x1052))
        .strings(&strings)
        .expect("valid static USB strings")
        .device_class(0)
        .max_packet_size_0(64)
        .expect("64-byte EP0 is valid for high-speed USB")
        .build();

    rprintln!(
        "USB1 cmd={:#010x} sts={:#010x} port={:#010x} otg={:#010x} mode={:#010x}",
        read32(USB1_USBCMD),
        read32(USB1_USBSTS),
        read32(USB1_PORTSC1),
        read32(USB1_OTGSC),
        read32(USB1_USBMODE)
    );
    rprintln!("OTG1 High-Speed HID waiting for PC enumeration");
    let mut configured = false;
    let mut idle_counter = 0u32;
    loop {
        let _ = device.poll(&mut [&mut hid]);
        if device.state() == UsbDeviceState::Configured {
            if !configured {
                device.bus().configure();
                configured = true;
                rprintln!("OTG1 HID configured by PC");
            }
            idle_counter = idle_counter.wrapping_add(1);
            if idle_counter & 0x3ffff == 0 {
                let _ = hid.push_mouse_extended(0, 0, 0, 0, 0);
            }
        } else {
            configured = false;
        }

        // Keep the compiler from turning this polling probe into a tight
        // sequence with no observable side effects between USB register reads.
        unsafe { read_volatile(SCB_VTOR as *const u32) };
    }
}

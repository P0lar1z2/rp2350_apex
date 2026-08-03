#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use imxrt_ral as ral;
use imxrt_usbd::{BusAdapter, EndpointMemory, EndpointState, Instances, Speed};
use panic_rtt_target as _;
use rt1052_bringup::{
    gamepad_converter::{
        APEX_DEFAULT_CONFIG, GAMEPAD_REPORT_DESCRIPTOR, GamepadReport, KbmToGamepad,
    },
    hid_report::{
        DecodedReport, KeyboardState, MouseState, ReportDecoder, parse_report_descriptor,
    },
    runtime_hid::{MAX_HID_INTERFACES, RuntimeHid},
};
use rtt_target::{ChannelMode::NoBlockSkip, rprintln, rtt_init_print};
use usb_device::{
    UsbError,
    bus::UsbBusAllocator,
    descriptor::lang_id::LangID,
    device::{StringDescriptors, UsbDeviceBuilder, UsbDeviceState, UsbRev, UsbVidPid},
};

const SCB_VTOR: usize = 0xE000_ED08;
const NVIC_ISER3: usize = 0xE000_E10C;
const NVIC_ICPR3: usize = 0xE000_E28C;
const NVIC_IPR_USB_OTG2: usize = 0xE000_E470;
const USB_OTG2_VECTOR: usize = (16 + 112) * 4;
const DEMCR: usize = 0xE000_EDFC;
const DWT_CTRL: usize = 0xE000_1000;
const DWT_CYCCNT: usize = 0xE000_1004;
const SOURCE_PROFILE_SETTLE_US: u32 = 1_000_000;
const GAMEPAD_KEEPALIVE_US: u32 = 1_000;
const CAP_KEYBOARD: u8 = 1 << 0;
const CAP_MOUSE: u8 = 1 << 1;

// Development-only placeholder. A distributed product needs an assigned VID/PID.
const GAMEPAD_VID: u16 = 0xcafe;
// Use a new development PID so Windows does not reuse the old DirectInput-only
// device node after the report descriptor changes to the XInputHID profile.
const GAMEPAD_PID: u16 = 0x1053;

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
    interface_index: u8,
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
            interface_index: 0,
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
    interface_number: u8,
    interface_index: u8,
}

impl HostReport {
    const fn empty() -> Self {
        Self {
            sequence: 0,
            length: 0,
            status: 0,
            data: [0; 64],
            interface_number: 0,
            interface_index: 0,
        }
    }
}

const _: () = assert!(core::mem::size_of::<HostEvent>() == 18);
const _: () = assert!(core::mem::size_of::<HostReport>() == 72);

unsafe extern "C" {
    fn nxp_host_init() -> i32;
    fn nxp_host_task();
    fn nxp_host_irq();
    fn nxp_host_pop_event(event: *mut HostEvent) -> i32;
    fn nxp_host_pop_report(report: *mut HostReport) -> i32;
    fn nxp_host_copy_report_descriptor(interface_index: u8, buffer: *mut u8, capacity: u16) -> i32;
    fn nxp_device_init_clocks() -> i32;
    fn nxp_core_clock_hz() -> u32;
}

struct CycleClock {
    last_cycle: u32,
    remainder: u32,
    micros: u32,
    cycles_per_us: u32,
}

impl CycleClock {
    fn new(core_hz: u32) -> Self {
        write32(
            DEMCR,
            unsafe { read_volatile(DEMCR as *const u32) } | (1 << 24),
        );
        write32(DWT_CYCCNT, 0);
        write32(
            DWT_CTRL,
            unsafe { read_volatile(DWT_CTRL as *const u32) } | 1,
        );
        Self {
            last_cycle: 0,
            remainder: 0,
            micros: 0,
            cycles_per_us: (core_hz / 1_000_000).max(1),
        }
    }

    fn now_us(&mut self) -> u32 {
        let cycle = unsafe { read_volatile(DWT_CYCCNT as *const u32) };
        let elapsed = cycle.wrapping_sub(self.last_cycle);
        self.last_cycle = cycle;
        let total = self.remainder.saturating_add(elapsed);
        self.micros = self.micros.wrapping_add(total / self.cycles_per_us);
        self.remainder = total % self.cycles_per_us;
        self.micros
    }
}

fn empty_mouse() -> MouseState {
    MouseState {
        buttons: 0,
        x: 0,
        y: 0,
        wheel: 0,
        pan: 0,
    }
}

fn capabilities(decoder: &ReportDecoder) -> u8 {
    let mut encoded = [0u8; 64];
    let mut result = 0;
    if decoder
        .encode_new(
            &DecodedReport::Keyboard(KeyboardState::empty()),
            &mut encoded,
        )
        .is_ok()
    {
        result |= CAP_KEYBOARD;
    }
    if decoder
        .encode_new(&DecodedReport::Mouse(empty_mouse()), &mut encoded)
        .is_ok()
    {
        result |= CAP_MOUSE;
    }
    result
}

fn select_sources(
    attached_mask: u8,
    sources: &[HostEvent; MAX_HID_INTERFACES],
    source_capabilities: &[u8; MAX_HID_INTERFACES],
) -> (Option<usize>, Option<usize>) {
    let mut keyboard = None;
    let mut mouse = None;
    for index in 0..MAX_HID_INTERFACES {
        if attached_mask & (1 << index) == 0 {
            continue;
        }
        let source = sources[index];
        if source_capabilities[index] & CAP_KEYBOARD != 0
            && keyboard.is_none_or(|current| {
                keyboard_rank(source, index) < keyboard_rank(sources[current], current)
            })
        {
            keyboard = Some(index);
        }
        if source_capabilities[index] & CAP_MOUSE != 0
            && mouse.is_none_or(|current| {
                mouse_rank(source, index) < mouse_rank(sources[current], current)
            })
        {
            mouse = Some(index);
        }
    }
    (keyboard, mouse)
}

fn keyboard_rank(source: HostEvent, index: usize) -> (u8, u16, usize) {
    (
        if source.interface_protocol == 1 { 0 } else { 1 },
        source.max_packet_size,
        index,
    )
}

fn mouse_rank(source: HostEvent, index: usize) -> (u8, u16, usize) {
    (
        if source.interface_protocol == 2 { 0 } else { 1 },
        source.max_packet_size,
        index,
    )
}

fn load_report_descriptor(
    index: usize,
    event: HostEvent,
    attached_mask: u8,
    descriptors: &mut [[u8; 512]; MAX_HID_INTERFACES],
    descriptor_lens: &mut [usize; MAX_HID_INTERFACES],
    decoders: &mut [ReportDecoder; MAX_HID_INTERFACES],
    source_capabilities: &mut [u8; MAX_HID_INTERFACES],
) {
    if index >= MAX_HID_INTERFACES || attached_mask & (1 << index) == 0 {
        return;
    }
    if event.status != 0 {
        descriptor_lens[index] = 0;
        decoders[index] = ReportDecoder::empty();
        source_capabilities[index] = 0;
        rprintln!(
            "OTG2 HID[{}] descriptor failed status={}",
            index,
            event.status
        );
        return;
    }

    // SAFETY: Destination capacity exactly matches the FFI argument.
    let length = unsafe {
        nxp_host_copy_report_descriptor(
            event.interface_index,
            descriptors[index].as_mut_ptr(),
            descriptors[index].len() as u16,
        )
    };
    let length = usize::try_from(length)
        .unwrap_or(0)
        .min(descriptors[index].len());
    descriptor_lens[index] = length;
    decoders[index] = parse_report_descriptor(&descriptors[index][..length]);
    source_capabilities[index] = capabilities(&decoders[index]);
    rprintln!(
        "OTG2 HID[{}] descriptor={} bytes capabilities={:#04x}",
        index,
        length,
        source_capabilities[index]
    );
}

fn clear_source(
    index: usize,
    attached_mask: &mut u8,
    sources: &mut [HostEvent; MAX_HID_INTERFACES],
    descriptor_lens: &mut [usize; MAX_HID_INTERFACES],
    decoders: &mut [ReportDecoder; MAX_HID_INTERFACES],
    source_capabilities: &mut [u8; MAX_HID_INTERFACES],
) {
    if index >= MAX_HID_INTERFACES {
        return;
    }
    *attached_mask &= !(1 << index);
    sources[index] = HostEvent::empty();
    descriptor_lens[index] = 0;
    decoders[index] = ReportDecoder::empty();
    source_capabilities[index] = 0;
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
    rt1052_bringup::prepare_runtime_memory();
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

    let core_hz = unsafe { nxp_core_clock_hz() };
    let mut clock = CycleClock::new(core_hz);
    let mut sources = [HostEvent::empty(); MAX_HID_INTERFACES];
    let mut descriptors = [[0u8; 512]; MAX_HID_INTERFACES];
    let mut descriptor_lens = [0usize; MAX_HID_INTERFACES];
    let mut decoders = [ReportDecoder::empty(); MAX_HID_INTERFACES];
    let mut source_capabilities = [0u8; MAX_HID_INTERFACES];
    let mut attached_mask = 0u8;
    let mut profile_stable_since = clock.now_us();

    rprintln!("waiting for one keyboard and one mouse on OTG2");
    loop {
        // SAFETY: Main-loop-only service of the initialized USB2 host stack.
        unsafe { nxp_host_task() };
        let mut event = HostEvent::empty();
        let mut profile_changed = false;
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            let index = usize::from(event.interface_index);
            match event.kind {
                1 if index < MAX_HID_INTERFACES => {
                    clear_source(
                        index,
                        &mut attached_mask,
                        &mut sources,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    sources[index] = event;
                    attached_mask |= 1 << index;
                    profile_changed = true;
                    rprintln!(
                        "OTG2 HID[{}] {:04x}:{:04x} if={} protocol={} packet={} interval={}",
                        index,
                        event.vid,
                        event.pid,
                        event.interface_number,
                        event.interface_protocol,
                        event.max_packet_size,
                        event.interval
                    );
                }
                2 => {
                    clear_source(
                        index,
                        &mut attached_mask,
                        &mut sources,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    profile_changed = true;
                    rprintln!("OTG2 HID[{}] detached", index);
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => {
                    rprintln!("OTG2 HID[{}] Interrupt IN ready", index)
                }
                4 => rprintln!(
                    "OTG2 HID[{}] receiver failed status={}",
                    index,
                    event.status
                ),
                5 => {
                    load_report_descriptor(
                        index,
                        event,
                        attached_mask,
                        &mut descriptors,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    profile_changed = true;
                }
                _ => {}
            }
        }

        let mut stale_report = HostReport::empty();
        // Do not fill the report queue while the source profiles settle.
        while unsafe { nxp_host_pop_report(&mut stale_report) } != 0 {}
        if profile_changed {
            profile_stable_since = clock.now_us();
        }
        let selected = select_sources(attached_mask, &sources, &source_capabilities);
        if selected.0.is_some()
            && selected.1.is_some()
            && clock.now_us().wrapping_sub(profile_stable_since) >= SOURCE_PROFILE_SETTLE_US
        {
            break;
        }
    }

    let (mut keyboard_source, mut mouse_source) =
        select_sources(attached_mask, &sources, &source_capabilities);
    rprintln!(
        "input sources ready: keyboard={:?} mouse={:?} core={} Hz",
        keyboard_source,
        mouse_source,
        core_hz
    );

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
    let bus = UsbBusAllocator::new(BusAdapter::with_speed(
        instances,
        &EP_MEMORY,
        &EP_STATE,
        Speed::LowFull,
    ));
    let neutral_report = GamepadReport::neutral().encode();
    let mut hid = RuntimeHid::new_bidirectional(
        &bus,
        GAMEPAD_REPORT_DESCRIPTOR,
        GamepadReport::LEN as u16,
        1,
        9,
        1,
        0,
        0,
    );
    hid.set_input_report(&neutral_report);
    let strings = [StringDescriptors::new(LangID::from(0x0409))
        .manufacturer("xense")
        .product("RT1052 XInputHID Gamepad")
        .serial_number("XENSE-XINPUTHID-0001")];
    let mut device = UsbDeviceBuilder::new(&bus, UsbVidPid(GAMEPAD_VID, GAMEPAD_PID))
        .usb_rev(UsbRev::Usb200)
        .device_release(0x0200)
        .max_power(100)
        .expect("gamepad USB power is valid")
        .max_packet_size_0(64)
        .expect("64-byte EP0 is valid")
        .strings(&strings)
        .expect("one English string table is valid")
        .build();

    let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
    let mut device_configured = false;
    let mut last_sent = neutral_report;
    let mut last_sent_valid = false;
    let mut last_sent_at = clock.now_us();
    let mut forwarded = 0u32;
    let mut send_errors = 0u32;
    rprintln!(
        "gamepad bridge running: report={} bytes interval=1 ms VID:PID={:04x}:{:04x}",
        GamepadReport::LEN,
        GAMEPAD_VID,
        GAMEPAD_PID
    );

    loop {
        // SAFETY: Main-loop-only service of the initialized USB2 host stack.
        unsafe { nxp_host_task() };
        let _ = device.poll(&mut [&mut hid]);
        if device.state() == UsbDeviceState::Configured {
            if !device_configured {
                device.bus().configure();
                device_configured = true;
                last_sent_valid = false;
                rprintln!("OTG1 gamepad configured by PC");
            }
        } else {
            device_configured = false;
            last_sent_valid = false;
        }

        let mut event = HostEvent::empty();
        let mut sources_changed = false;
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            let index = usize::from(event.interface_index);
            match event.kind {
                1 if index < MAX_HID_INTERFACES => {
                    if keyboard_source == Some(index) {
                        converter.release_keyboard();
                        keyboard_source = None;
                    }
                    if mouse_source == Some(index) {
                        converter.release_mouse();
                        mouse_source = None;
                    }
                    clear_source(
                        index,
                        &mut attached_mask,
                        &mut sources,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    sources[index] = event;
                    attached_mask |= 1 << index;
                    sources_changed = true;
                    rprintln!(
                        "OTG2 HID[{}] attached {:04x}:{:04x} protocol={}",
                        index,
                        event.vid,
                        event.pid,
                        event.interface_protocol
                    );
                }
                2 => {
                    if keyboard_source == Some(index) {
                        converter.release_keyboard();
                        keyboard_source = None;
                    }
                    if mouse_source == Some(index) {
                        converter.release_mouse();
                        mouse_source = None;
                    }
                    clear_source(
                        index,
                        &mut attached_mask,
                        &mut sources,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    sources_changed = true;
                    rprintln!("OTG2 HID[{}] detached; owned gamepad state released", index);
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => {
                    rprintln!("OTG2 HID[{}] Interrupt IN ready", index)
                }
                4 => rprintln!(
                    "OTG2 HID[{}] receiver failed status={}",
                    index,
                    event.status
                ),
                5 => {
                    load_report_descriptor(
                        index,
                        event,
                        attached_mask,
                        &mut descriptors,
                        &mut descriptor_lens,
                        &mut decoders,
                        &mut source_capabilities,
                    );
                    sources_changed = true;
                }
                _ => {}
            }
        }
        if sources_changed {
            let selected = select_sources(attached_mask, &sources, &source_capabilities);
            if selected.0 != keyboard_source {
                converter.release_keyboard();
                keyboard_source = selected.0;
                rprintln!("keyboard source changed to {:?}", keyboard_source);
            }
            if selected.1 != mouse_source {
                converter.release_mouse();
                mouse_source = selected.1;
                rprintln!("mouse source changed to {:?}", mouse_source);
            }
        }

        for _ in 0..32 {
            let mut report = HostReport::empty();
            // SAFETY: `report` is writable storage matching the C ABI.
            if unsafe { nxp_host_pop_report(&mut report) } == 0 {
                break;
            }
            let index = usize::from(report.interface_index);
            let length = usize::from(report.length.min(64));
            if index >= MAX_HID_INTERFACES || report.status != 0 || length == 0 {
                continue;
            }
            let Some(decoded) = decoders[index].decode(&report.data[..length]) else {
                continue;
            };
            match decoded {
                DecodedReport::Keyboard(state) if keyboard_source == Some(index) => {
                    converter.observe_keyboard(state);
                }
                DecodedReport::Mouse(state) if mouse_source == Some(index) => {
                    let now = clock.now_us();
                    converter.observe_mouse(state, now);
                }
                _ => {}
            }
        }

        let now = clock.now_us();
        converter.tick(now);
        let desired_state = converter.report();
        let desired = desired_state.encode();
        let keepalive_due = now.wrapping_sub(last_sent_at) >= GAMEPAD_KEEPALIVE_US;
        if device_configured && (!last_sent_valid || desired != last_sent || keepalive_due) {
            match hid.push_report(&desired) {
                Ok(_) => {
                    last_sent = desired;
                    last_sent_valid = true;
                    last_sent_at = now;
                    converter.acknowledge_report();
                    forwarded = forwarded.wrapping_add(1);
                    if forwarded <= 8 || forwarded & 1023 == 0 {
                        rprintln!(
                            "gamepad #{} lx={} ly={} rx={} ry={} buttons={:#06x} hat={} lt={} rt={} errors={}",
                            forwarded,
                            desired_state.left_x,
                            desired_state.left_y,
                            desired_state.right_x,
                            desired_state.right_y,
                            desired_state.buttons,
                            desired_state.hat,
                            desired_state.left_trigger,
                            desired_state.right_trigger,
                            send_errors
                        );
                    }
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => send_errors = send_errors.wrapping_add(1),
            }
        }

        // Preserve an observable MMIO read in this polling loop.
        unsafe { read_volatile(SCB_VTOR as *const u32) };
    }
}

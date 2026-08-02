#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use imxrt_ral as ral;
use imxrt_usbd::{BusAdapter, EndpointMemory, EndpointState, Instances, Speed};
use panic_rtt_target as _;
use rt1052_bringup::{
    hid_report::{
        DecodedReport, KeyboardState, MouseState, ReportDecoder, parse_report_descriptor,
    },
    macro_config::CONFIG as MACRO_CONFIG,
    macro_engine::{MacroEngine, MacroMouseReport},
    runtime_hid::{HidReportProxy, MAX_HID_INTERFACES, RuntimeCompositeHid, RuntimeHidInterface},
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
const USB1_USBCMD: usize = 0x402E_0140;
const SOURCE_PROFILE_SETTLE_US: u32 = 1_000_000;

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

const _: () = assert!(core::mem::size_of::<HostEvent>() == 18);
const _: () = assert!(core::mem::size_of::<HostReport>() == 72);

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

unsafe extern "C" {
    fn nxp_host_init() -> i32;
    fn nxp_host_task();
    fn nxp_host_irq();
    fn nxp_host_pop_event(event: *mut HostEvent) -> i32;
    fn nxp_host_pop_report(report: *mut HostReport) -> i32;
    fn nxp_host_copy_report_descriptor(interface_index: u8, buffer: *mut u8, capacity: u16) -> i32;
    fn nxp_host_copy_device_descriptor(interface_index: u8, buffer: *mut u8, capacity: u16) -> i32;
    fn nxp_host_copy_configuration_descriptor(
        interface_index: u8,
        buffer: *mut u8,
        capacity: u16,
    ) -> i32;
    fn nxp_host_begin_string_descriptor(
        interface_index: u8,
        descriptor_index: u8,
        language_id: u16,
    ) -> i32;
    fn nxp_host_copy_string_descriptor(buffer: *mut u8, capacity: u16) -> i32;
    fn nxp_host_hid_get_report(
        interface_index: u8,
        report_id: u8,
        report_type: u8,
        buffer: *mut u8,
        capacity: u16,
    ) -> i32;
    fn nxp_host_hid_set_report(
        interface_index: u8,
        report_id: u8,
        report_type: u8,
        buffer: *const u8,
        length: u16,
    ) -> i32;
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

fn merge_mouse(physical: MouseState, generated: MacroMouseReport) -> MouseState {
    MouseState {
        buttons: generated.buttons,
        x: physical.x.saturating_add(generated.x),
        y: physical.y.saturating_add(generated.y),
        wheel: i16::from(physical.wheel)
            .saturating_add(i16::from(generated.wheel))
            .clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8,
        pan: i16::from(physical.pan)
            .saturating_add(i16::from(generated.pan))
            .clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8,
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

fn same_attached_device(actual: HostEvent, expected: HostEvent) -> bool {
    actual.address == expected.address
        && actual.hub_address == expected.hub_address
        && actual.hub_port == expected.hub_port
        && actual.vid == expected.vid
        && actual.pid == expected.pid
}

fn presentation_device_index(
    attached_mask: u8,
    sources: &[HostEvent; MAX_HID_INTERFACES],
    descriptors: &[[u8; 512]; MAX_HID_INTERFACES],
    descriptor_lens: &[usize; MAX_HID_INTERFACES],
) -> Option<usize> {
    for candidate_index in 0..MAX_HID_INTERFACES {
        if attached_mask & (1 << candidate_index) == 0 {
            continue;
        }
        let candidate = sources[candidate_index];
        let mut keyboard = false;
        let mut mouse = false;
        let mut encoded = [0u8; 64];
        for index in 0..MAX_HID_INTERFACES {
            let length = descriptor_lens[index];
            if attached_mask & (1 << index) == 0
                || length == 0
                || !same_attached_device(sources[index], candidate)
            {
                continue;
            }
            let decoder = parse_report_descriptor(&descriptors[index][..length]);
            keyboard |= decoder
                .encode_new(
                    &DecodedReport::Keyboard(KeyboardState::empty()),
                    &mut encoded,
                )
                .is_ok();
            mouse |= decoder
                .encode_new(&DecodedReport::Mouse(empty_mouse()), &mut encoded)
                .is_ok();
        }
        if keyboard && mouse {
            return Some(candidate_index);
        }
    }
    None
}

fn same_source_profile(actual: HostEvent, expected: HostEvent) -> bool {
    actual.vid == expected.vid
        && actual.pid == expected.pid
        && actual.speed == expected.speed
        && actual.interface_number == expected.interface_number
        && actual.interface_subclass == expected.interface_subclass
        && actual.interface_protocol == expected.interface_protocol
        && actual.endpoint_address == expected.endpoint_address
        && actual.max_packet_size == expected.max_packet_size
        && actual.interval == expected.interval
}

fn keyboard_source_rank(
    source: HostEvent,
    encoded_report_len: usize,
    descriptor_len: usize,
) -> (u8, u16, usize, usize) {
    /* Prefer a boot-keyboard interface, then the smallest interrupt packet.
     * This selects the dedicated Dell 8-byte keyboard over auxiliary gaming-
     * device keyboard collections that also decode as keyboard-capable. */
    (
        if source.interface_protocol == 1 { 0 } else { 1 },
        source.max_packet_size,
        encoded_report_len,
        descriptor_len,
    )
}

fn keyboard_target_rank(
    source: HostEvent,
    encoded_report_len: usize,
    descriptor_len: usize,
) -> (u8, usize, usize, u16) {
    /* Gaming devices can expose several protocol-1 keyboard interfaces with
     * the same 64-byte endpoint size. Prefer the interface whose actual
     * keyboard report is smallest, then the simpler descriptor. This selects
     * the Razer boot-keyboard interface instead of its complex auxiliary
     * keyboard collection. */
    (
        if source.interface_protocol == 1 { 0 } else { 1 },
        encoded_report_len,
        descriptor_len,
        source.max_packet_size,
    )
}

fn keyboard_has_input(state: KeyboardState) -> bool {
    state.modifiers != 0 || state.keys.iter().any(|keys| *keys != 0)
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from(bytes[offset]) | (u16::from(bytes[offset + 1]) << 8)
}

fn hid_metadata(configuration: &[u8], interface_number: u8) -> (u16, u8) {
    let mut offset = 0usize;
    let mut matching_interface = false;
    while offset + 2 <= configuration.len() {
        let length = usize::from(configuration[offset]);
        if length < 2 || offset + length > configuration.len() {
            break;
        }
        match configuration[offset + 1] {
            0x04 if length >= 9 => {
                matching_interface =
                    configuration[offset + 2] == interface_number && configuration[offset + 3] == 0;
            }
            0x21 if matching_interface && length >= 6 => {
                return (le_u16(configuration, offset + 2), configuration[offset + 4]);
            }
            _ => {}
        }
        offset += length;
    }
    (0x0111, 0)
}

fn interface_string_index(configuration: &[u8], interface_number: u8) -> u8 {
    let mut offset = 0usize;
    while offset + 2 <= configuration.len() {
        let length = usize::from(configuration[offset]);
        if length < 2 || offset + length > configuration.len() {
            break;
        }
        if configuration[offset + 1] == 0x04
            && length >= 9
            && configuration[offset + 2] == interface_number
            && configuration[offset + 3] == 0
        {
            return configuration[offset + 8];
        }
        offset += length;
    }
    0
}

fn fetch_string_descriptor(
    interface_index: u8,
    descriptor_index: u8,
    language_id: u16,
    buffer: &mut [u8; 255],
    clock: &mut CycleClock,
) -> usize {
    // SAFETY: The interface exists for the profile lifetime and the C shim owns
    // its transfer storage until nxp_host_copy_string_descriptor completes.
    if unsafe { nxp_host_begin_string_descriptor(interface_index, descriptor_index, language_id) }
        == 0
    {
        return 0;
    }
    let started = clock.now_us();
    loop {
        // SAFETY: Main-loop-only service of the initialized USB2 host stack.
        unsafe { nxp_host_task() };
        // SAFETY: The destination capacity exactly matches the FFI argument.
        let result =
            unsafe { nxp_host_copy_string_descriptor(buffer.as_mut_ptr(), buffer.len() as u16) };
        if result >= 0 {
            return usize::try_from(result).unwrap_or(0).min(buffer.len());
        }
        if clock.now_us().wrapping_sub(started) >= 1_000_000 {
            return 0;
        }
    }
}

fn decode_usb_string(descriptor: &[u8], output: &mut [u8]) -> usize {
    if descriptor.len() < 2 || descriptor[1] != 0x03 {
        return 0;
    }
    let descriptor_len = usize::from(descriptor[0]).min(descriptor.len());
    let mut source = 2usize;
    let mut target = 0usize;
    while source + 1 < descriptor_len {
        let first = le_u16(descriptor, source);
        source += 2;
        let character = if (0xd800..=0xdbff).contains(&first) && source + 1 < descriptor_len {
            let second = le_u16(descriptor, source);
            if (0xdc00..=0xdfff).contains(&second) {
                source += 2;
                char::from_u32(
                    0x1_0000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second) - 0xdc00,
                )
                .unwrap_or(char::REPLACEMENT_CHARACTER)
            } else {
                char::REPLACEMENT_CHARACTER
            }
        } else {
            char::from_u32(u32::from(first)).unwrap_or(char::REPLACEMENT_CHARACTER)
        };
        let mut encoded = [0u8; 4];
        let bytes = character.encode_utf8(&mut encoded).as_bytes();
        if target + bytes.len() > output.len() {
            break;
        }
        output[target..target + bytes.len()].copy_from_slice(bytes);
        target += bytes.len();
    }
    target
}

fn proxy_get_report(
    interface_index: u8,
    report_id: u8,
    report_type: u8,
    buffer: &mut [u8],
) -> Option<usize> {
    // SAFETY: The C shim owns the downstream HID control transfer and checks
    // the interface and buffer capacity before writing.
    let length = unsafe {
        nxp_host_hid_get_report(
            interface_index,
            report_id,
            report_type,
            buffer.as_mut_ptr(),
            buffer.len() as u16,
        )
    };
    usize::try_from(length)
        .ok()
        .map(|length| length.min(buffer.len()))
}

fn proxy_set_report(interface_index: u8, report_id: u8, report_type: u8, report: &[u8]) -> bool {
    // SAFETY: The C shim copies the report into DMA-safe storage before the
    // downstream control transfer and checks the supplied length.
    unsafe {
        nxp_host_hid_set_report(
            interface_index,
            report_id,
            report_type,
            report.as_ptr(),
            report.len() as u16,
        ) != 0
    }
}

#[inline]
fn write32(address: usize, value: u32) {
    // SAFETY: Callers provide valid, aligned RT1052 register/vector addresses.
    unsafe { write_volatile(address as *mut u32, value) }
}

fn set_upstream_attached(attached: bool) {
    let command = unsafe { read_volatile(USB1_USBCMD as *const u32) };
    write32(
        USB1_USBCMD,
        if attached { command | 1 } else { command & !1 },
    );
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
    rprintln!("waiting for OTG2 keyboard and mouse profiles before attaching OTG1");
    let mut sources = [HostEvent::empty(); MAX_HID_INTERFACES];
    let mut report_descriptors = [[0u8; 512]; MAX_HID_INTERFACES];
    let mut report_descriptor_lens = [0usize; MAX_HID_INTERFACES];
    let mut attached_mask = 0u8;
    let mut descriptor_done_mask = 0u8;
    let mut presentation_ready = false;
    let mut profile_stable_since = clock.now_us();
    'profile: loop {
        // SAFETY: Called only from this main loop; USB2 IRQ handles controller events.
        unsafe { nxp_host_task() };
        let mut event = HostEvent::empty();
        let mut profile_changed = false;
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => {
                    let index = usize::from(event.interface_index);
                    if index < MAX_HID_INTERFACES {
                        sources[index] = event;
                        attached_mask |= 1 << index;
                        profile_changed = true;
                    }
                    rprintln!(
                        "source[{}] if={} {:04x}:{:04x} speed={} subclass={} protocol={} packet={} interval={}",
                        event.interface_index,
                        event.interface_number,
                        event.vid,
                        event.pid,
                        event.speed,
                        event.interface_subclass,
                        event.interface_protocol,
                        event.max_packet_size,
                        event.interval
                    );
                }
                2 => {
                    let index = usize::from(event.interface_index);
                    if index < MAX_HID_INTERFACES {
                        attached_mask &= !(1 << index);
                        descriptor_done_mask &= !(1 << index);
                        report_descriptor_lens[index] = 0;
                        sources[index] = HostEvent::empty();
                        profile_changed = true;
                    }
                    rprintln!(
                        "source[{}] detached while reading profile",
                        event.interface_index
                    );
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => {
                    rprintln!("OTG2 HID[{}] Interrupt IN ready", event.interface_index)
                }
                4 => rprintln!(
                    "OTG2 HID[{}] receiver failed status={}",
                    event.interface_index,
                    event.status
                ),
                5 => {
                    let index = usize::from(event.interface_index);
                    if index < MAX_HID_INTERFACES && attached_mask & (1 << index) != 0 {
                        if event.status == 0 {
                            // SAFETY: Destination capacity matches the FFI argument.
                            let length = unsafe {
                                nxp_host_copy_report_descriptor(
                                    event.interface_index,
                                    report_descriptors[index].as_mut_ptr(),
                                    512,
                                )
                            };
                            report_descriptor_lens[index] = usize::try_from(length)
                                .unwrap_or(0)
                                .min(report_descriptors[index].len());
                            rprintln!(
                                "OTG2 HID[{}] descriptor={} bytes",
                                index,
                                report_descriptor_lens[index]
                            );
                        } else {
                            rprintln!(
                                "OTG2 HID[{}] descriptor failed status={}",
                                index,
                                event.status
                            );
                        }
                        descriptor_done_mask |= 1 << index;
                        profile_changed = true;
                    }
                }
                6 => rprintln!(
                    "source[{}] if={} endpoint={:#04x} attributes={:#04x} packet={} interval={}",
                    event.interface_index,
                    event.interface_number,
                    event.endpoint_address,
                    event.status,
                    event.max_packet_size,
                    event.interval
                ),
                _ => {}
            }
        }
        let mut stale_report = HostReport::empty();
        // Avoid filling the shared queue while all interface descriptors arrive.
        while unsafe { nxp_host_pop_report(&mut stale_report) } != 0 {}
        if profile_changed {
            presentation_ready = false;
            profile_stable_since = clock.now_us();
            if attached_mask != 0 && descriptor_done_mask == attached_mask {
                presentation_ready = presentation_device_index(
                    attached_mask,
                    &sources,
                    &report_descriptors,
                    &report_descriptor_lens,
                )
                .is_some();
            }
        }
        if presentation_ready
            && clock.now_us().wrapping_sub(profile_stable_since) >= SOURCE_PROFILE_SETTLE_US
        {
            break 'profile;
        }
    }

    let identity_index = presentation_device_index(
        attached_mask,
        &sources,
        &report_descriptors,
        &report_descriptor_lens,
    )
    .expect("a HID device with keyboard and mouse reports");
    let identity = sources[identity_index];
    let mut device_descriptor = [0u8; 18];
    // SAFETY: The selected interface remains attached and the destination is
    // the exact size of a standard USB device descriptor.
    let device_descriptor_len = unsafe {
        nxp_host_copy_device_descriptor(
            identity.interface_index,
            device_descriptor.as_mut_ptr(),
            device_descriptor.len() as u16,
        )
    };
    let mut configuration_descriptor = [0u8; 512];
    // SAFETY: The selected interface remains attached and the shim checks the
    // destination capacity before copying the raw active configuration.
    let configuration_descriptor_len = unsafe {
        nxp_host_copy_configuration_descriptor(
            identity.interface_index,
            configuration_descriptor.as_mut_ptr(),
            configuration_descriptor.len() as u16,
        )
    };
    let configuration_descriptor_len = usize::try_from(configuration_descriptor_len)
        .unwrap_or(0)
        .min(configuration_descriptor.len());
    let configuration_descriptor = &configuration_descriptor[..configuration_descriptor_len];
    if device_descriptor_len != device_descriptor.len() as i32 || configuration_descriptor_len < 9 {
        rprintln!(
            "Razer identity snapshot failed: device={} config={}",
            device_descriptor_len,
            configuration_descriptor_len
        );
    }
    let mut raw_string = [0u8; 255];
    let language_len =
        fetch_string_descriptor(identity.interface_index, 0, 0, &mut raw_string, &mut clock);
    let language_id = if language_len >= 4 && raw_string[1] == 0x03 {
        le_u16(&raw_string, 2)
    } else {
        0x0409
    };
    let manufacturer_index = device_descriptor.get(14).copied().unwrap_or(0);
    let product_index = device_descriptor.get(15).copied().unwrap_or(0);
    let serial_index = device_descriptor.get(16).copied().unwrap_or(0);
    let mut manufacturer_utf8 = [0u8; 256];
    let mut product_utf8 = [0u8; 256];
    let mut serial_utf8 = [0u8; 256];
    let manufacturer_len = if manufacturer_index == 0 {
        0
    } else {
        let length = fetch_string_descriptor(
            identity.interface_index,
            manufacturer_index,
            language_id,
            &mut raw_string,
            &mut clock,
        );
        decode_usb_string(&raw_string[..length], &mut manufacturer_utf8)
    };
    let product_len = if product_index == 0 {
        0
    } else {
        let length = fetch_string_descriptor(
            identity.interface_index,
            product_index,
            language_id,
            &mut raw_string,
            &mut clock,
        );
        decode_usb_string(&raw_string[..length], &mut product_utf8)
    };
    let serial_len = if serial_index == 0 {
        0
    } else {
        let length = fetch_string_descriptor(
            identity.interface_index,
            serial_index,
            language_id,
            &mut raw_string,
            &mut clock,
        );
        decode_usb_string(&raw_string[..length], &mut serial_utf8)
    };
    let manufacturer = core::str::from_utf8(&manufacturer_utf8[..manufacturer_len]).ok();
    let product = core::str::from_utf8(&product_utf8[..product_len]).ok();
    let serial = core::str::from_utf8(&serial_utf8[..serial_len]).ok();
    let mut interface_string_utf8 = [[0u8; 256]; MAX_HID_INTERFACES];
    let mut interface_string_lens = [0usize; MAX_HID_INTERFACES];
    for index in 0..MAX_HID_INTERFACES {
        if attached_mask & (1 << index) == 0 || !same_attached_device(sources[index], identity) {
            continue;
        }
        let string_index =
            interface_string_index(configuration_descriptor, sources[index].interface_number);
        if string_index == 0 {
            continue;
        }
        let length = fetch_string_descriptor(
            identity.interface_index,
            string_index,
            language_id,
            &mut raw_string,
            &mut clock,
        );
        interface_string_lens[index] =
            decode_usb_string(&raw_string[..length], &mut interface_string_utf8[index]);
    }
    let interface_strings: [Option<&str>; MAX_HID_INTERFACES] = core::array::from_fn(|index| {
        let length = interface_string_lens[index];
        (length != 0)
            .then(|| core::str::from_utf8(&interface_string_utf8[index][..length]).ok())
            .flatten()
    });
    rprintln!(
        "Razer identity: device={:02x?} config_len={} lang={:#06x} manufacturer={:?} product={:?} serial={:?}",
        device_descriptor,
        configuration_descriptor_len,
        language_id,
        manufacturer,
        product,
        serial
    );
    let mut stale_report = HostReport::empty();
    while unsafe { nxp_host_pop_report(&mut stale_report) } != 0 {}
    let profiles: [Option<RuntimeHidInterface>; MAX_HID_INTERFACES] = core::array::from_fn(
        |index| {
            let source = sources[index];
            let descriptor_len = report_descriptor_lens[index];
            if attached_mask & (1 << index) == 0
                || descriptor_len == 0
                || source.max_packet_size > 64
                || !same_attached_device(source, identity)
            {
                return None;
            }
            let interval = source.interval.max(1);
            let (hid_version, country_code) =
                hid_metadata(configuration_descriptor, source.interface_number);
            rprintln!(
                "profile[{}]: if={} descriptor={} packet={} FS interval={} protocol={} HID={:#06x} country={}",
                index,
                source.interface_number,
                descriptor_len,
                source.max_packet_size,
                interval,
                source.interface_protocol,
                hid_version,
                country_code
            );
            /* `main` never returns and this storage is not modified after the
             * profile phase, so the descriptor remains valid for every later
             * EP0 request. RuntimeHidInterface requires this lifetime to use
             * usb-device's zero-copy control-IN path for descriptors >256 B. */
            let report_descriptor: &'static [u8] =
                unsafe { core::mem::transmute(&report_descriptors[index][..descriptor_len]) };
            /* Like the report descriptor, decoded interface strings live in
             * `main` for the entire firmware lifetime and are immutable after
             * this profile-construction phase. */
            let interface_string: Option<&'static str> =
                unsafe { core::mem::transmute(interface_strings[index]) };
            Some(RuntimeHidInterface {
                report_descriptor,
                max_packet_size: source.max_packet_size,
                interval,
                subclass: source.interface_subclass,
                protocol: source.interface_protocol,
                hid_version,
                country_code,
                interface_string,
                language_id: LangID::from(language_id),
            })
        },
    );
    let active_count = profiles.iter().flatten().count();
    if active_count == 0 {
        rprintln!("no cloneable HID interfaces (descriptor required, packet <= 64)");
        loop {
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
    let bus = UsbBusAllocator::new(BusAdapter::with_speed(
        instances,
        &EP_MEMORY,
        &EP_STATE,
        Speed::LowFull,
    ));
    let mut hid = RuntimeCompositeHid::new(
        &bus,
        profiles,
        Some(HidReportProxy {
            get_report: proxy_get_report,
            set_report: proxy_set_report,
        }),
    );
    let mut string_descriptor = StringDescriptors::new(LangID::from(language_id));
    if let Some(value) = manufacturer {
        string_descriptor = string_descriptor.manufacturer(value);
    }
    if let Some(value) = product {
        string_descriptor = string_descriptor.product(value);
    }
    if let Some(value) = serial {
        string_descriptor = string_descriptor.serial_number(value);
    }
    let strings = [string_descriptor];
    let usb_revision = if device_descriptor_len == device_descriptor.len() as i32
        && le_u16(&device_descriptor, 2) == 0x0200
    {
        UsbRev::Usb200
    } else {
        UsbRev::Usb210
    };
    let device_class = device_descriptor.get(4).copied().unwrap_or(0);
    let device_subclass = device_descriptor.get(5).copied().unwrap_or(0);
    let device_protocol = device_descriptor.get(6).copied().unwrap_or(0);
    let ep0_packet_size = device_descriptor.get(7).copied().unwrap_or(64);
    let device_release = if device_descriptor_len == device_descriptor.len() as i32 {
        le_u16(&device_descriptor, 12)
    } else {
        0x0010
    };
    let configuration_attributes = configuration_descriptor.get(7).copied().unwrap_or(0x80);
    let max_power_ma = usize::from(configuration_descriptor.get(8).copied().unwrap_or(50)) * 2;
    let mut builder = UsbDeviceBuilder::new(&bus, UsbVidPid(identity.vid, identity.pid))
        .device_class(device_class)
        .device_sub_class(device_subclass)
        .device_protocol(device_protocol)
        .usb_rev(usb_revision)
        .device_release(device_release)
        .self_powered(configuration_attributes & 0x40 != 0)
        .supports_remote_wakeup(configuration_attributes & 0x20 != 0)
        .max_power(max_power_ma)
        .expect("downstream USB power is valid")
        .max_packet_size_0(ep0_packet_size)
        .expect("downstream EP0 packet size is valid");
    if manufacturer.is_some() || product.is_some() || serial.is_some() {
        builder = builder
            .strings(&strings)
            .expect("one downstream USB language is valid");
    }
    let mut device = builder.build();

    rprintln!(
        "dynamic composite bridge running: {} OTG2 HID interfaces -> OTG1",
        active_count
    );
    let decoders: [ReportDecoder; MAX_HID_INTERFACES] = core::array::from_fn(|index| {
        let length = report_descriptor_lens[index];
        if length == 0 {
            ReportDecoder::empty()
        } else {
            parse_report_descriptor(&report_descriptors[index][..length])
        }
    });
    let mut keyboard_interface: Option<usize> = None;
    let mut keyboard_source_interface: Option<usize> = None;
    let mut keyboard_report_lens: [Option<usize>; MAX_HID_INTERFACES] = [None; MAX_HID_INTERFACES];
    let mut keyboard_template = [0u8; 64];
    let mut keyboard_template_len = 0usize;
    let mut mouse_interface = None;
    let mut mouse_template = [0u8; 64];
    let mut mouse_template_len = 0usize;
    let empty_keyboard = DecodedReport::Keyboard(KeyboardState::empty());
    let empty_mouse = DecodedReport::Mouse(empty_mouse());
    for (index, decoder) in decoders.iter().enumerate() {
        let mut encoded_keyboard = [0u8; 64];
        if let Ok(length) = decoder.encode_new(&empty_keyboard, &mut encoded_keyboard) {
            keyboard_report_lens[index] = Some(length);
            let preferred_source = keyboard_source_interface.is_none_or(|current| {
                keyboard_source_rank(sources[index], length, report_descriptor_lens[index])
                    < keyboard_source_rank(
                        sources[current],
                        keyboard_report_lens[current].unwrap_or(usize::MAX),
                        report_descriptor_lens[current],
                    )
            });
            if preferred_source {
                keyboard_source_interface = Some(index);
            }
            let preferred_target = profiles[index].is_some()
                && keyboard_interface.is_none_or(|current| {
                    keyboard_target_rank(sources[index], length, report_descriptor_lens[index])
                        < keyboard_target_rank(
                            sources[current],
                            keyboard_report_lens[current].unwrap_or(usize::MAX),
                            report_descriptor_lens[current],
                        )
                });
            if preferred_target {
                keyboard_interface = Some(index);
                keyboard_template[..length].copy_from_slice(&encoded_keyboard[..length]);
                keyboard_template_len = length;
            }
        }
        if profiles[index].is_some()
            && mouse_interface.is_none()
            && let Ok(length) = decoder.encode_new(&empty_mouse, &mut mouse_template)
        {
            mouse_interface = Some(index);
            mouse_template_len = length;
        }
    }
    let macro_seed = u64::from(core_hz) ^ (u64::from(identity.vid) << 32) ^ u64::from(identity.pid);
    let mut macro_engine = MacroEngine::new(&MACRO_CONFIG, macro_seed);
    rprintln!(
        "macro engine ready: core={} Hz keyboard_source={:?}/{} keyboard_target={:?}/{} mouse={:?}",
        core_hz,
        keyboard_source_interface,
        keyboard_source_interface
            .and_then(|index| keyboard_report_lens[index])
            .unwrap_or(0),
        keyboard_interface,
        keyboard_interface
            .and_then(|index| keyboard_report_lens[index])
            .unwrap_or(0),
        mouse_interface
    );
    let mut device_configured = false;
    let mut forwarded = [0u32; MAX_HID_INTERFACES];
    let mut dropped = [0u32; MAX_HID_INTERFACES];
    let mut pending = [[0u8; 64]; MAX_HID_INTERFACES];
    let mut pending_len = [0usize; MAX_HID_INTERFACES];
    let mut pending_ready = [false; MAX_HID_INTERFACES];
    let mut last_sent = [[0u8; 64]; MAX_HID_INTERFACES];
    let mut last_sent_len = [0usize; MAX_HID_INTERFACES];
    let mut last_sent_valid = [false; MAX_HID_INTERFACES];
    let mut source_connected = true;
    let profile_mask = profiles
        .iter()
        .enumerate()
        .fold(0u8, |mask, (index, profile)| {
            if profile.is_some() {
                mask | (1 << index)
            } else {
                mask
            }
        });
    let mut current_sources = sources;
    let mut host_to_source: [Option<usize>; MAX_HID_INTERFACES] = core::array::from_fn(|index| {
        if attached_mask & (1 << index) != 0 {
            Some(index)
        } else {
            None
        }
    });
    let mut reconnect_descriptor = [0u8; 512];

    loop {
        // SAFETY: Called only from this main loop; USB2 IRQ handles controller events.
        unsafe { nxp_host_task() };
        let _ = device.poll(&mut [&mut hid]);
        if device.state() == UsbDeviceState::Configured {
            if !device_configured {
                device.bus().configure();
                device_configured = true;
                rprintln!("OTG1 composite bridge configured by PC");
            }
        } else {
            if device_configured {
                last_sent_valid.fill(false);
            }
            device_configured = false;
        }

        for index in 0..MAX_HID_INTERFACES {
            if !pending_ready[index] || !device_configured {
                continue;
            }
            match hid.push_report(index, &pending[index][..pending_len[index]]) {
                Ok(_) => {
                    last_sent[index][..pending_len[index]]
                        .copy_from_slice(&pending[index][..pending_len[index]]);
                    last_sent_len[index] = pending_len[index];
                    last_sent_valid[index] = true;
                    pending_ready[index] = false;
                    forwarded[index] = forwarded[index].wrapping_add(1);
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    pending_ready[index] = false;
                    dropped[index] = dropped[index].wrapping_add(1);
                }
            }
        }

        macro_engine.tick(clock.now_us());
        if device_configured {
            if let Some(index) = keyboard_interface
                && !pending_ready[index]
                && macro_engine.has_keyboard_output()
                && let Some(generated) = macro_engine.take_keyboard_output()
            {
                pending[index][..keyboard_template_len]
                    .copy_from_slice(&keyboard_template[..keyboard_template_len]);
                if decoders[index]
                    .encode(
                        &DecodedReport::Keyboard(generated),
                        &mut pending[index][..keyboard_template_len],
                    )
                    .is_ok()
                {
                    pending_len[index] = keyboard_template_len;
                    pending_ready[index] = true;
                }
            }
            if let Some(index) = mouse_interface
                && !pending_ready[index]
                && macro_engine.has_mouse_output()
                && let Some(generated) = macro_engine.take_mouse_output()
            {
                pending[index][..mouse_template_len]
                    .copy_from_slice(&mouse_template[..mouse_template_len]);
                let generated = DecodedReport::Mouse(MouseState {
                    buttons: generated.buttons,
                    x: generated.x,
                    y: generated.y,
                    wheel: generated.wheel,
                    pan: generated.pan,
                });
                if decoders[index]
                    .encode(&generated, &mut pending[index][..mouse_template_len])
                    .is_ok()
                {
                    pending_len[index] = mouse_template_len;
                    pending_ready[index] = true;
                }
            }
        }

        let mut event = HostEvent::empty();
        // SAFETY: `event` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_event(&mut event) } != 0 {
            match event.kind {
                1 => {
                    let index = usize::from(event.interface_index);
                    if index < MAX_HID_INTERFACES {
                        current_sources[index] = event;
                        host_to_source[index] = None;
                    }
                    rprintln!(
                        "OTG2 HID[{}] {:04x}:{:04x} addr={} hub={}:{} speed={} ep={:#04x} interval={}",
                        event.interface_index,
                        event.vid,
                        event.pid,
                        event.address,
                        event.hub_address,
                        event.hub_port,
                        event.speed,
                        event.endpoint_address,
                        event.interval
                    );
                }
                2 => {
                    let host_index = usize::from(event.interface_index);
                    let removed_source = if host_index < MAX_HID_INTERFACES {
                        current_sources[host_index] = HostEvent::empty();
                        host_to_source[host_index].take()
                    } else {
                        None
                    };
                    let removed_presentation =
                        removed_source.is_some_and(|index| profiles[index].is_some());
                    if let Some(source_index) = removed_source {
                        hid.set_downstream_interface(source_index, None);
                    }
                    if removed_presentation && source_connected {
                        set_upstream_attached(false);
                        source_connected = false;
                        device_configured = false;
                        pending_ready.fill(false);
                        last_sent_valid.fill(false);
                        macro_engine = MacroEngine::new(&MACRO_CONFIG, macro_seed);
                        rprintln!(
                            "OTG2 HID[{}] profile {:?} detached; OTG1 disconnected from PC",
                            event.interface_index,
                            removed_source
                        );
                    } else if removed_source == keyboard_source_interface {
                        keyboard_source_interface = None;
                        macro_engine = MacroEngine::new(&MACRO_CONFIG, macro_seed);
                        if let Some(index) = keyboard_interface {
                            pending[index][..keyboard_template_len]
                                .copy_from_slice(&keyboard_template[..keyboard_template_len]);
                            if decoders[index]
                                .encode(
                                    &DecodedReport::Keyboard(KeyboardState::empty()),
                                    &mut pending[index][..keyboard_template_len],
                                )
                                .is_ok()
                            {
                                pending_len[index] = keyboard_template_len;
                                pending_ready[index] = true;
                            }
                        }
                        rprintln!(
                            "OTG2 HID[{}] keyboard source detached; translated state released",
                            event.interface_index
                        );
                    } else {
                        rprintln!("OTG2 HID[{}] detached", event.interface_index);
                    }
                }
                3 => rprintln!("OTG2 enumeration failed status={}", event.status),
                4 if event.status == 0 => {
                    rprintln!("OTG2 HID[{}] Interrupt IN ready", event.interface_index)
                }
                4 => rprintln!(
                    "OTG2 HID[{}] receiver failed status={}",
                    event.interface_index,
                    event.status
                ),
                5 if event.status == 0 => {
                    let host_index = usize::from(event.interface_index);
                    if host_index >= MAX_HID_INTERFACES {
                        continue;
                    }
                    let length = unsafe {
                        nxp_host_copy_report_descriptor(
                            event.interface_index,
                            reconnect_descriptor.as_mut_ptr(),
                            512,
                        )
                    };
                    let length = usize::try_from(length).unwrap_or(0);
                    let matched_source = (0..MAX_HID_INTERFACES).find(|&source_index| {
                        attached_mask & (1 << source_index) != 0
                            && !host_to_source.contains(&Some(source_index))
                            && same_source_profile(
                                current_sources[host_index],
                                sources[source_index],
                            )
                            && length == report_descriptor_lens[source_index]
                            && reconnect_descriptor[..length]
                                == report_descriptors[source_index][..length]
                    });
                    host_to_source[host_index] = matched_source;
                    if let Some(source_index) = matched_source {
                        hid.set_downstream_interface(source_index, Some(event.interface_index));
                    }
                    rprintln!(
                        "OTG2 HID[{}] matched source profile {:?}",
                        event.interface_index,
                        matched_source
                    );
                }
                5 => {
                    rprintln!(
                        "OTG2 HID[{}] descriptor failed status={}",
                        event.interface_index,
                        event.status
                    );
                }
                6 => rprintln!(
                    "OTG2 HID[{}] endpoint={:#04x} attributes={:#04x} packet={} interval={}",
                    event.interface_index,
                    event.endpoint_address,
                    event.status,
                    event.max_packet_size,
                    event.interval
                ),
                _ => {}
            }
        }
        let mapped_profile_mask = host_to_source.iter().fold(0u8, |mask, source| {
            source.map_or(mask, |index| mask | (1 << index))
        });
        if !source_connected && mapped_profile_mask & profile_mask == profile_mask {
            set_upstream_attached(true);
            source_connected = true;
            rprintln!("all OTG2 HID profiles restored; OTG1 reattached");
        }

        if pending_ready.iter().any(|ready| *ready) {
            continue;
        }
        let mut report = HostReport::empty();
        // SAFETY: `report` is writable storage matching the C ABI.
        while unsafe { nxp_host_pop_report(&mut report) } != 0 {
            let host_index = usize::from(report.interface_index);
            let Some(source_index) = host_to_source.get(host_index).copied().flatten() else {
                continue;
            };
            if !source_connected {
                continue;
            }
            let length = usize::from(report.length.min(64));
            if report.status != 0 || length == 0 {
                continue;
            }
            if !device_configured {
                dropped[source_index] = dropped[source_index].wrapping_add(1);
                continue;
            }
            let decoded = decoders[source_index].decode(&report.data[..length]);
            let target_index = match decoded {
                Some(DecodedReport::Keyboard(_)) if profiles[source_index].is_none() => {
                    let Some(index) = keyboard_interface else {
                        continue;
                    };
                    index
                }
                _ if profiles[source_index].is_some() => source_index,
                _ => continue,
            };
            let translated = target_index != source_index;
            let target_length = if translated {
                pending[target_index][..keyboard_template_len]
                    .copy_from_slice(&keyboard_template[..keyboard_template_len]);
                keyboard_template_len
            } else {
                pending[target_index][..length].copy_from_slice(&report.data[..length]);
                length
            };
            if let Some(original) = decoded {
                if let DecodedReport::Keyboard(state) = original
                    && keyboard_has_input(state)
                    && keyboard_report_lens[source_index].is_some()
                    && keyboard_source_interface.is_none_or(|current| {
                        keyboard_source_rank(
                            sources[source_index],
                            keyboard_report_lens[source_index].unwrap_or(usize::MAX),
                            report_descriptor_lens[source_index],
                        ) < keyboard_source_rank(
                            sources[current],
                            keyboard_report_lens[current].unwrap_or(usize::MAX),
                            report_descriptor_lens[current],
                        )
                    })
                {
                    keyboard_source_interface = Some(source_index);
                    rprintln!("macro keyboard source switched to profile {}", source_index);
                }
                let is_macro_source = match original {
                    DecodedReport::Keyboard(_) => keyboard_source_interface == Some(source_index),
                    DecodedReport::Mouse(_) => mouse_interface == Some(source_index),
                    DecodedReport::Consumer(_) => false,
                };
                let mut transformed = if is_macro_source {
                    let transformed = macro_engine.observe(original);
                    // Run newly triggered programs before this physical report
                    // is presented upstream. This lets a masked held key (for
                    // example W in the Shift+W lurch trigger) transfer directly
                    // to synthetic ownership without an intervening key-up.
                    macro_engine.tick(clock.now_us());
                    transformed
                } else {
                    original
                };
                let mut needs_encode = translated || transformed != original;
                match transformed {
                    DecodedReport::Keyboard(state) if keyboard_interface == Some(target_index) => {
                        let generated = macro_engine.keyboard_output();
                        needs_encode |= generated != state;
                        transformed = DecodedReport::Keyboard(generated);
                        // This target report now carries the current engine
                        // state, so suppress a redundant periodic report.
                        let _ = macro_engine.take_keyboard_output();
                    }
                    DecodedReport::Mouse(physical) if mouse_interface == Some(target_index) => {
                        mouse_template[..target_length]
                            .copy_from_slice(&pending[target_index][..target_length]);
                        mouse_template_len = target_length;
                        if let Some(generated) = macro_engine.take_mouse_output() {
                            needs_encode |= generated.x != 0
                                || generated.y != 0
                                || generated.wheel != 0
                                || generated.pan != 0
                                || generated.buttons != physical.buttons;
                            transformed = DecodedReport::Mouse(merge_mouse(physical, generated));
                        }
                    }
                    _ => {}
                }
                if needs_encode {
                    let _ = decoders[target_index]
                        .encode(&transformed, &mut pending[target_index][..target_length]);
                }
                match transformed {
                    DecodedReport::Keyboard(_) if keyboard_interface == Some(target_index) => {
                        keyboard_template[..target_length]
                            .copy_from_slice(&pending[target_index][..target_length]);
                    }
                    DecodedReport::Mouse(_) if mouse_interface == Some(target_index) => {
                        mouse_template[..target_length]
                            .copy_from_slice(&pending[target_index][..target_length]);
                    }
                    _ => {}
                }
            }
            pending_len[target_index] = target_length;
            if translated
                && last_sent_valid[target_index]
                && last_sent_len[target_index] == target_length
                && last_sent[target_index][..target_length]
                    == pending[target_index][..target_length]
            {
                pending_ready[target_index] = false;
                continue;
            }
            pending_ready[target_index] = true;
            match hid.push_report(target_index, &pending[target_index][..target_length]) {
                Ok(_) => {
                    last_sent[target_index][..target_length]
                        .copy_from_slice(&pending[target_index][..target_length]);
                    last_sent_len[target_index] = target_length;
                    last_sent_valid[target_index] = true;
                    pending_ready[target_index] = false;
                    forwarded[target_index] = forwarded[target_index].wrapping_add(1);
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    pending_ready[target_index] = false;
                    dropped[target_index] = dropped[target_index].wrapping_add(1);
                }
            }
            if forwarded[target_index] <= 8 || forwarded[target_index] & 127 == 0 {
                match decoded {
                    Some(DecodedReport::Mouse(mouse)) => rprintln!(
                        "forward[{}<-{}] #{} buttons={:#04x} x={} y={} wheel={} pan={} dropped={}",
                        target_index,
                        source_index,
                        forwarded[target_index],
                        mouse.buttons,
                        mouse.x,
                        mouse.y,
                        mouse.wheel,
                        mouse.pan,
                        dropped[target_index]
                    ),
                    Some(DecodedReport::Keyboard(keyboard)) => rprintln!(
                        "forward[{}<-{}] #{} modifiers={:#04x} keys={:02x?} dropped={}",
                        target_index,
                        source_index,
                        forwarded[target_index],
                        keyboard.modifiers,
                        keyboard.keys,
                        dropped[target_index]
                    ),
                    _ => rprintln!(
                        "forward[{}<-{}] #{} raw_len={} dropped={}",
                        target_index,
                        source_index,
                        forwarded[target_index],
                        target_length,
                        dropped[target_index]
                    ),
                }
            }
            if pending_ready[target_index] {
                break;
            }
        }

        // Preserve an observable MMIO read in this polling loop.
        unsafe { read_volatile(SCB_VTOR as *const u32) };
    }
}

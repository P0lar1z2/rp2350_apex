#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use imxrt_ral as ral;
use imxrt_usbd::{BusAdapter, EndpointMemory, EndpointState, Instances};
use panic_rtt_target as _;
use rt1052_bringup::{
    control_protocol::{self, ControlCommand},
    enet_device::EnetDevice,
    hid_report::{
        DecodedReport, KeyboardState, MouseState, ReportDecoder, parse_report_descriptor,
    },
    macro_config::CONFIG as MACRO_CONFIG,
    macro_engine::{DEFAULT_GAME_SENSITIVITY_MILLI, MacroEngine, MacroMouseReport},
    runtime_hid::{
        MAX_HID_INTERFACES, RuntimeCompositeHid, RuntimeHidInterface, high_speed_interval,
    },
};
use rtt_target::{ChannelMode::NoBlockSkip, rprintln, rtt_init_print};
use smoltcp::{
    iface::{Config as NetworkConfig, Interface, SocketHandle, SocketSet, SocketStorage},
    socket::{dhcpv4, udp},
    time::Instant,
    wire::{EthernetAddress, IpCidr},
};
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
const DEMCR: usize = 0xE000_EDFC;
const DWT_CTRL: usize = 0xE000_1000;
const DWT_CYCCNT: usize = 0xE000_1004;
const USB1_USBCMD: usize = 0x402E_0140;
const SOURCE_PROFILE_SETTLE_US: u32 = 1_000_000;
const CONTROL_UDP_PORT: u16 = 1052;
const ACK_OK: u8 = 0;
const ACK_UNSUPPORTED: u8 = 1;

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

fn service_control_endpoint(
    interface: &mut Interface,
    enet: &mut EnetDevice,
    sockets: &mut SocketSet<'_>,
    dhcp_handle: SocketHandle,
    udp_handle: SocketHandle,
    now_us: u32,
    last_sequence: &mut Option<u32>,
) -> Option<ControlCommand> {
    let now = Instant::from_millis(i64::from(now_us / 1_000));
    let _ = interface.poll(now, enet, sockets);

    match sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll() {
        Some(dhcpv4::Event::Configured(config)) => {
            interface.update_ip_addrs(|addresses| {
                addresses.clear();
                addresses.push(IpCidr::Ipv4(config.address)).unwrap();
            });
            if let Some(router) = config.router {
                let _ = interface.routes_mut().add_default_ipv4_route(router);
            } else {
                interface.routes_mut().remove_default_ipv4_route();
            }
            rprintln!(
                "READY: DHCP IP={} gateway={:?} RTCP UDP {}",
                config.address,
                config.router,
                CONTROL_UDP_PORT
            );
        }
        Some(dhcpv4::Event::Deconfigured) => {
            interface.update_ip_addrs(|addresses| addresses.clear());
            interface.routes_mut().remove_default_ipv4_route();
            rprintln!("DHCP lease lost; RTCP inactive");
        }
        None => {}
    }

    let socket = sockets.get_mut::<udp::Socket>(udp_handle);
    if !socket.can_recv() {
        return None;
    }
    let mut datagram = [0u8; control_protocol::HEADER_LEN + control_protocol::MAX_PAYLOAD_LEN];
    let Ok((length, remote)) = socket.recv_slice(&mut datagram) else {
        return None;
    };
    let Ok(frame) = control_protocol::decode(&datagram[..length]) else {
        rprintln!("RTCP rejected malformed datagram len={}", length);
        return None;
    };
    let duplicate = *last_sequence == Some(frame.sequence);
    let (status, command) = match frame.command {
        command @ (ControlCommand::SetSensitivityMilli(_)
        | ControlCommand::SetKey { .. }
        | ControlCommand::SetMouseButtons(_)
        | ControlCommand::MoveMouse { .. }
        | ControlCommand::EmergencyRelease) => (ACK_OK, Some(command)),
        _ => (ACK_UNSUPPORTED, None),
    };
    let mut ack = [0u8; 16];
    if let Ok(ack_len) = control_protocol::encode_ack(&mut ack, frame.sequence, datagram[5], status)
        && socket.can_send()
    {
        let _ = socket.send_slice(&ack[..ack_len], remote.endpoint);
    }
    if status == ACK_OK {
        *last_sequence = Some(frame.sequence);
    }
    if duplicate {
        rprintln!("RTCP seq={} duplicate ACK", frame.sequence);
        None
    } else if let Some(command) = command {
        rprintln!("RTCP seq={} command={:?}", frame.sequence, command);
        Some(command)
    } else {
        rprintln!(
            "RTCP seq={} unsupported command={:?}",
            frame.sequence,
            frame.command
        );
        None
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

fn source_profiles_include_keyboard_and_mouse(
    attached_mask: u8,
    descriptors: &[[u8; 512]; MAX_HID_INTERFACES],
    descriptor_lens: &[usize; MAX_HID_INTERFACES],
) -> bool {
    let mut keyboard = false;
    let mut mouse = false;
    let mut encoded = [0u8; 64];

    for index in 0..MAX_HID_INTERFACES {
        let length = descriptor_lens[index];
        if attached_mask & (1 << index) == 0 || length == 0 {
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

    keyboard && mouse
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

    let mut enet = match EnetDevice::new() {
        Ok(enet) => enet,
        Err(error) => {
            rprintln!("ERROR: ENET init={}", error);
            loop {
                cortex_m::asm::nop();
            }
        }
    };
    let enet_status = enet.status().unwrap_or_default();
    rprintln!(
        "ENET PHY {:04x}:{:04x} link={} {}M {} duplex",
        enet_status.phy_id1,
        enet_status.phy_id2,
        enet_status.link_up,
        if enet_status.speed_100m != 0 { 100 } else { 10 },
        if enet_status.full_duplex != 0 {
            "full"
        } else {
            "half"
        }
    );

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
    let mac = EthernetAddress([0x02, 0x10, 0x52, 0x00, 0x00, 0x01]);
    let mut network_config = NetworkConfig::new(mac.into());
    network_config.random_seed = 0x1052_0001;
    let network_now = Instant::from_millis(i64::from(clock.now_us() / 1_000));
    let mut network = Interface::new(network_config, &mut enet, network_now);
    let mut udp_rx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_rx_data = [0u8; 256];
    let mut udp_tx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut udp_tx_data = [0u8; 256];
    let udp_rx_buffer = udp::PacketBuffer::new(&mut udp_rx_meta[..], &mut udp_rx_data[..]);
    let udp_tx_buffer = udp::PacketBuffer::new(&mut udp_tx_meta[..], &mut udp_tx_data[..]);
    let mut udp_socket = udp::Socket::new(udp_rx_buffer, udp_tx_buffer);
    udp_socket.bind(CONTROL_UDP_PORT).unwrap();
    let mut socket_storage = [SocketStorage::EMPTY; 2];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);
    let udp_handle = sockets.add(udp_socket);
    let dhcp_handle = sockets.add(dhcpv4::Socket::new());
    let mut requested_sensitivity_milli = DEFAULT_GAME_SENSITIVITY_MILLI;
    let mut last_control_sequence = None;
    rprintln!(
        "DHCP discovering; RTCP sensitivity control UDP {}",
        CONTROL_UDP_PORT
    );
    rprintln!("waiting for OTG2 keyboard and mouse profiles before attaching OTG1");
    let mut sources = [HostEvent::empty(); MAX_HID_INTERFACES];
    let mut report_descriptors = [[0u8; 512]; MAX_HID_INTERFACES];
    let mut report_descriptor_lens = [0usize; MAX_HID_INTERFACES];
    let mut attached_mask = 0u8;
    let mut descriptor_done_mask = 0u8;
    let mut source_roles_ready = false;
    let mut profile_stable_since = clock.now_us();
    'profile: loop {
        if let Some(ControlCommand::SetSensitivityMilli(value)) = service_control_endpoint(
            &mut network,
            &mut enet,
            &mut sockets,
            dhcp_handle,
            udp_handle,
            clock.now_us(),
            &mut last_control_sequence,
        ) {
            requested_sensitivity_milli = value;
        }
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
            source_roles_ready = false;
            profile_stable_since = clock.now_us();
            if attached_mask != 0 && descriptor_done_mask == attached_mask {
                source_roles_ready = source_profiles_include_keyboard_and_mouse(
                    attached_mask,
                    &report_descriptors,
                    &report_descriptor_lens,
                );
            }
        }
        if source_roles_ready
            && clock.now_us().wrapping_sub(profile_stable_since) >= SOURCE_PROFILE_SETTLE_US
        {
            break 'profile;
        }
    }

    let identity = sources
        .iter()
        .copied()
        .find(|source| source.max_packet_size != 0)
        .expect("at least one HID source");
    let profiles: [Option<RuntimeHidInterface>; MAX_HID_INTERFACES] =
        core::array::from_fn(|index| {
            let source = sources[index];
            let descriptor_len = report_descriptor_lens[index];
            if attached_mask & (1 << index) == 0
                || descriptor_len == 0
                || source.max_packet_size > 64
            {
                return None;
            }
            let interval = high_speed_interval(source.speed, source.interval);
            rprintln!(
                "profile[{}]: if={} descriptor={} packet={} HS interval={} protocol={}",
                index,
                source.interface_number,
                descriptor_len,
                source.max_packet_size,
                interval,
                source.interface_protocol
            );
            /* `main` never returns and this storage is not modified after the
             * profile phase, so the descriptor remains valid for every later
             * EP0 request. RuntimeHidInterface requires this lifetime to use
             * usb-device's zero-copy control-IN path for descriptors >256 B. */
            let report_descriptor: &'static [u8] =
                unsafe { core::mem::transmute(&report_descriptors[index][..descriptor_len]) };
            Some(RuntimeHidInterface {
                report_descriptor,
                max_packet_size: source.max_packet_size,
                interval,
                subclass: source.interface_subclass,
                protocol: source.interface_protocol,
            })
        });
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
    let bus = UsbBusAllocator::new(BusAdapter::new(instances, &EP_MEMORY, &EP_STATE));
    let mut hid = RuntimeCompositeHid::new(&bus, profiles);
    let strings = [StringDescriptors::default()
        .manufacturer("xense")
        .product("RT1052 Composite HID Clone")
        .serial_number("RAM-COMPOSITE")];
    let mut device = UsbDeviceBuilder::new(&bus, UsbVidPid(identity.vid, identity.pid))
        .strings(&strings)
        .expect("valid static USB strings")
        .device_class(0)
        .max_packet_size_0(64)
        .expect("64-byte EP0 is valid for high-speed USB")
        .build();

    rprintln!(
        "dynamic composite clone running: {} OTG2 HID interfaces -> OTG1",
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
    let mut keyboard_interface = None;
    let mut keyboard_template = [0u8; 64];
    let mut keyboard_template_len = 0usize;
    let mut mouse_interface = None;
    let mut mouse_template = [0u8; 64];
    let mut mouse_template_len = 0usize;
    let empty_keyboard = DecodedReport::Keyboard(KeyboardState::empty());
    let empty_mouse = DecodedReport::Mouse(empty_mouse());
    for (index, decoder) in decoders.iter().enumerate() {
        if keyboard_interface.is_none()
            && let Ok(length) = decoder.encode_new(&empty_keyboard, &mut keyboard_template)
        {
            keyboard_interface = Some(index);
            keyboard_template_len = length;
        }
        if mouse_interface.is_none()
            && let Ok(length) = decoder.encode_new(&empty_mouse, &mut mouse_template)
        {
            mouse_interface = Some(index);
            mouse_template_len = length;
        }
    }
    let macro_seed = u64::from(core_hz) ^ (u64::from(identity.vid) << 32) ^ u64::from(identity.pid);
    let mut macro_engine = MacroEngine::new(&MACRO_CONFIG, macro_seed);
    let _ = macro_engine.set_game_sensitivity_milli(requested_sensitivity_milli);
    rprintln!(
        "macro engine ready: core={} Hz keyboard={:?} mouse={:?} sensitivity={}.{:03}",
        core_hz,
        keyboard_interface,
        mouse_interface,
        requested_sensitivity_milli / 1_000,
        requested_sensitivity_milli % 1_000
    );
    let mut device_configured = false;
    let mut forwarded = [0u32; MAX_HID_INTERFACES];
    let mut dropped = [0u32; MAX_HID_INTERFACES];
    let mut pending = [[0u8; 64]; MAX_HID_INTERFACES];
    let mut pending_len = [0usize; MAX_HID_INTERFACES];
    let mut pending_ready = [false; MAX_HID_INTERFACES];
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
    let mut host_to_profile: [Option<usize>; MAX_HID_INTERFACES] =
        core::array::from_fn(|index| profiles[index].map(|_| index));
    let mut reconnect_descriptor = [0u8; 512];

    loop {
        if let Some(command) = service_control_endpoint(
            &mut network,
            &mut enet,
            &mut sockets,
            dhcp_handle,
            udp_handle,
            clock.now_us(),
            &mut last_control_sequence,
        ) {
            match command {
                ControlCommand::SetSensitivityMilli(value) => {
                    requested_sensitivity_milli = value;
                    let _ = macro_engine.set_game_sensitivity_milli(value);
                }
                ControlCommand::SetKey { usage, pressed } => {
                    let _ = macro_engine.set_remote_key(usage, pressed);
                }
                ControlCommand::SetMouseButtons(buttons) => {
                    macro_engine.set_remote_mouse_buttons(buttons);
                }
                ControlCommand::MoveMouse { x, y, wheel, pan } => {
                    macro_engine.push_remote_mouse_motion(x, y, wheel, pan);
                }
                ControlCommand::EmergencyRelease => macro_engine.emergency_release_remote(),
                _ => {}
            }
        }
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
            device_configured = false;
        }

        for index in 0..MAX_HID_INTERFACES {
            if !pending_ready[index] || !device_configured {
                continue;
            }
            match hid.push_report(index, &pending[index][..pending_len[index]]) {
                Ok(_) => {
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
                        host_to_profile[index] = None;
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
                    let removed_profile = if host_index < MAX_HID_INTERFACES {
                        current_sources[host_index] = HostEvent::empty();
                        host_to_profile[host_index].take()
                    } else {
                        None
                    };
                    if removed_profile.is_some() && source_connected {
                        set_upstream_attached(false);
                        source_connected = false;
                        device_configured = false;
                        pending_ready.fill(false);
                        macro_engine = MacroEngine::new(&MACRO_CONFIG, macro_seed);
                        let _ =
                            macro_engine.set_game_sensitivity_milli(requested_sensitivity_milli);
                        rprintln!(
                            "OTG2 HID[{}] profile {:?} detached; OTG1 disconnected from PC",
                            event.interface_index,
                            removed_profile
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
                    let matched_profile = (0..MAX_HID_INTERFACES).find(|&profile_index| {
                        profile_mask & (1 << profile_index) != 0
                            && !host_to_profile.contains(&Some(profile_index))
                            && same_source_profile(
                                current_sources[host_index],
                                sources[profile_index],
                            )
                            && length == report_descriptor_lens[profile_index]
                            && reconnect_descriptor[..length]
                                == report_descriptors[profile_index][..length]
                    });
                    host_to_profile[host_index] = matched_profile;
                    rprintln!(
                        "OTG2 HID[{}] matched upstream profile {:?}",
                        event.interface_index,
                        matched_profile
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
        let mapped_profile_mask = host_to_profile.iter().fold(0u8, |mask, profile| {
            profile.map_or(mask, |index| mask | (1 << index))
        });
        if !source_connected && mapped_profile_mask == profile_mask {
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
            let Some(index) = host_to_profile.get(host_index).copied().flatten() else {
                continue;
            };
            if !source_connected || profiles[index].is_none() {
                continue;
            }
            let length = usize::from(report.length.min(64));
            if report.status != 0 || length == 0 {
                continue;
            }
            if !device_configured {
                dropped[index] = dropped[index].wrapping_add(1);
                continue;
            }
            pending[index][..length].copy_from_slice(&report.data[..length]);
            let decoded = decoders[index].decode(&pending[index][..length]);
            if let Some(original) = decoded {
                let mut transformed = macro_engine.observe(original);
                let mut needs_encode = transformed != original;
                match transformed {
                    DecodedReport::Keyboard(state) => {
                        keyboard_interface = Some(index);
                        keyboard_template[..length].copy_from_slice(&pending[index][..length]);
                        keyboard_template_len = length;
                        if let Some(generated) = macro_engine.take_keyboard_output() {
                            needs_encode |= generated != state;
                            transformed = DecodedReport::Keyboard(generated);
                        }
                    }
                    DecodedReport::Mouse(physical) => {
                        mouse_interface = Some(index);
                        mouse_template[..length].copy_from_slice(&pending[index][..length]);
                        mouse_template_len = length;
                        if let Some(generated) = macro_engine.take_mouse_output() {
                            needs_encode |= generated.x != 0
                                || generated.y != 0
                                || generated.wheel != 0
                                || generated.pan != 0
                                || generated.buttons != physical.buttons;
                            transformed = DecodedReport::Mouse(merge_mouse(physical, generated));
                        }
                    }
                    DecodedReport::Consumer(_) => {}
                }
                if needs_encode {
                    let _ = decoders[index].encode(&transformed, &mut pending[index][..length]);
                }
                match transformed {
                    DecodedReport::Keyboard(_) => {
                        keyboard_template[..length].copy_from_slice(&pending[index][..length])
                    }
                    DecodedReport::Mouse(_) => {
                        mouse_template[..length].copy_from_slice(&pending[index][..length]);
                    }
                    DecodedReport::Consumer(_) => {}
                }
            }
            pending_len[index] = length;
            pending_ready[index] = true;
            match hid.push_report(index, &pending[index][..length]) {
                Ok(_) => {
                    pending_ready[index] = false;
                    forwarded[index] = forwarded[index].wrapping_add(1);
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    pending_ready[index] = false;
                    dropped[index] = dropped[index].wrapping_add(1);
                }
            }
            if forwarded[index] <= 8 || forwarded[index] & 127 == 0 {
                match decoded {
                    Some(DecodedReport::Mouse(mouse)) => rprintln!(
                        "clone[{}] #{} buttons={:#04x} x={} y={} wheel={} pan={} dropped={}",
                        index,
                        forwarded[index],
                        mouse.buttons,
                        mouse.x,
                        mouse.y,
                        mouse.wheel,
                        mouse.pan,
                        dropped[index]
                    ),
                    Some(DecodedReport::Keyboard(keyboard)) => rprintln!(
                        "clone[{}] #{} modifiers={:#04x} keys={:02x?} dropped={}",
                        index,
                        forwarded[index],
                        keyboard.modifiers,
                        keyboard.keys,
                        dropped[index]
                    ),
                    _ => rprintln!(
                        "clone[{}] #{} raw_len={} dropped={}",
                        index,
                        forwarded[index],
                        length,
                        dropped[index]
                    ),
                }
            }
            if pending_ready[index] {
                break;
            }
        }

        // Preserve an observable MMIO read in this polling loop.
        unsafe { read_volatile(SCB_VTOR as *const u32) };
    }
}

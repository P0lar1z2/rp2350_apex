#![no_std]
#![no_main]

use core::ptr::{read_volatile, write_volatile};

use cortex_m_rt::entry;
use panic_rtt_target as _;
use rtt_target::{rprintln, rtt_init_print};
use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpCidr};

use rt1052_bringup::control_protocol::{self, ControlQueue};
use rt1052_bringup::enet_device::EnetDevice;

const UDP_PORT: u16 = 1052;
const CORE_HZ: u64 = 528_000_000;

struct CycleClock {
    last: u32,
    total: u64,
}

impl CycleClock {
    fn new() -> Self {
        const DEMCR: usize = 0xE000_EDFC;
        const DWT_CTRL: usize = 0xE000_1000;
        const DWT_CYCCNT: usize = 0xE000_1004;
        // SAFETY: These are the Cortex-M7 trace and DWT cycle counter registers.
        unsafe {
            write_volatile(DEMCR as *mut u32, read_volatile(DEMCR as *const u32) | (1 << 24));
            write_volatile(DWT_CYCCNT as *mut u32, 0);
            write_volatile(DWT_CTRL as *mut u32, read_volatile(DWT_CTRL as *const u32) | 1);
        }
        Self { last: 0, total: 0 }
    }

    fn now(&mut self) -> Instant {
        // SAFETY: DWT_CYCCNT is an aligned read-only use of the cycle counter.
        let current = unsafe { read_volatile(0xE000_1004 as *const u32) };
        self.total = self.total.wrapping_add(current.wrapping_sub(self.last) as u64);
        self.last = current;
        Instant::from_millis((self.total / (CORE_HZ / 1_000)) as i64)
    }
}

#[entry]
fn main() -> ! {
    rt1052_bringup::use_nxp_default_flexram();
    cortex_m::interrupt::disable();
    // SAFETY: This RAM image's vector table starts at ITCM address zero.
    unsafe { write_volatile(0xE000_ED08 as *mut u32, 0) };
    rtt_init_print!();
    rprintln!("RT1052 asynchronous Ethernet control");

    let mut device = match EnetDevice::new() {
        Ok(device) => device,
        Err(error) => {
            rprintln!("ERROR: ENET init={}", error);
            loop { cortex_m::asm::nop(); }
        }
    };
    let status = device.status().unwrap_or_default();
    rprintln!(
        "PHY {:04x}:{:04x} link={} {}M {} duplex",
        status.phy_id1, status.phy_id2, status.link_up,
        if status.speed_100m != 0 { 100 } else { 10 },
        if status.full_duplex != 0 { "full" } else { "half" }
    );

    let mut clock = CycleClock::new();
    let mac = EthernetAddress([0x02, 0x10, 0x52, 0x00, 0x00, 0x01]);
    let mut config = Config::new(mac.into());
    config.random_seed = 0x1052_0001;
    let mut iface = Interface::new(config, &mut device, clock.now());

    let mut rx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut rx_data = [0u8; 256];
    let mut tx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut tx_data = [0u8; 256];
    let rx_buffer = udp::PacketBuffer::new(&mut rx_meta[..], &mut rx_data[..]);
    let tx_buffer = udp::PacketBuffer::new(&mut tx_meta[..], &mut tx_data[..]);
    let mut udp_socket = udp::Socket::new(rx_buffer, tx_buffer);
    udp_socket.bind(UDP_PORT).unwrap();
    let mut socket_storage = [SocketStorage::EMPTY; 2];
    let mut sockets = SocketSet::new(&mut socket_storage[..]);
    let udp_handle = sockets.add(udp_socket);
    let dhcp_handle = sockets.add(dhcpv4::Socket::new());
    let mut queue = ControlQueue::<8>::new();
    let mut datagram = [0u8; control_protocol::HEADER_LEN + control_protocol::MAX_PAYLOAD_LEN];
    let mut ack = [0u8; 16];

    rprintln!("DHCP: discovering; UDP {} will activate after lease", UDP_PORT);
    loop {
        let _ = iface.poll(clock.now(), &mut device, &mut sockets);

        let dhcp_event = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
        match dhcp_event {
            Some(dhcpv4::Event::Configured(config)) => {
                iface.update_ip_addrs(|addresses| {
                    addresses.clear();
                    addresses.push(IpCidr::Ipv4(config.address)).unwrap();
                });
                if let Some(router) = config.router {
                    let _ = iface.routes_mut().add_default_ipv4_route(router);
                } else {
                    iface.routes_mut().remove_default_ipv4_route();
                }
                rprintln!("READY: DHCP IP={} gateway={:?} UDP {}", config.address, config.router, UDP_PORT);
            }
            Some(dhcpv4::Event::Deconfigured) => {
                iface.update_ip_addrs(|addresses| addresses.clear());
                iface.routes_mut().remove_default_ipv4_route();
                rprintln!("DHCP: lease lost; control socket inactive");
            }
            None => {}
        }

        let socket = sockets.get_mut::<udp::Socket>(udp_handle);
        if socket.can_recv() {
            if let Ok((length, remote)) = socket.recv_slice(&mut datagram) {
                match control_protocol::decode(&datagram[..length]) {
                    Ok(frame) => {
                        rprintln!("RTCP seq={} command={:?}", frame.sequence, frame.command);
                        let _ = queue.push(frame);
                        if let Ok(ack_len) = control_protocol::encode_ack(&mut ack, frame.sequence, datagram[5], 0) {
                            if socket.can_send() {
                                let _ = socket.send_slice(&ack[..ack_len], remote.endpoint);
                            }
                        }
                    }
                    Err(error) => rprintln!("RTCP reject len={} error={:?}", length, error),
                }
            }
        }
        // Probe executor: dequeue asynchronously. The integrated bridge will map
        // these commands to MacroEngine outside all USB and ENET IRQ paths.
        if let Some(frame) = queue.pop() {
            rprintln!("EXEC seq={} command={:?} dropped={}", frame.sequence, frame.command, queue.dropped());
        }
    }
}

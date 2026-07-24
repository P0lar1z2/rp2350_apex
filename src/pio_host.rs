//! Blocking full/low-speed USB transactions on the RP2350 PIO implementation.

use hal::{
    pac::{PIO0, PIO1},
    pio::{PIO, PIO0SM1, PIO1SM0, PIO1SM1, Running, Rx, StateMachine, Tx},
    timer::{Timer, TimerDevice},
};
use pio::{
    Instruction, InstructionOperands, JmpCondition, MovDestination, MovOperation, MovSource,
    SetDestination,
};
use rp235x_hal as hal;
use t2::usb_host::{
    PacketError, ReceivedPacket, UsbPid, build_data_packet, build_token_packet, crc16_step,
    parse_received_packet,
};

/// Upper bound for busy-wait loops on PIO state. At 144 MHz this is tens of
/// milliseconds — far beyond any legal bus turnaround, so expiry means a
/// wedged state machine, which must surface as a Timeout instead of hanging
/// the main loop (and with it the Type-C device and debug telemetry).
const SPIN_LIMIT: u32 = 2_000_000;

const RX_IRQ_EOP: u8 = 1 << 2;
const RX_IRQ_START: u8 = 1 << 3;
const RX_IRQ_TRIGGER: u8 = 1 << 4;
const TX_IRQ_EOP: u8 = 1 << 1;
const FS_J_SIDE: u8 = 0b01;
const LS_J_SIDE: u8 = 0b10;

// The TX program is installed at origin 0; `out pc, 2` jumps to one of its
// first four instructions. Address 4 (`start`) re-enables the pin drivers and
// is entered with a forced jump before each packet.
const TX_SYMBOL_EOP: u8 = 0;
const TX_SYMBOL_DRIVE_J: u8 = 1;
const TX_SYMBOL_RELEASE: u8 = 2;
const TX_SYMBOL_DRIVE_K: u8 = 3;
const TX_START_ADDRESS: u8 = 4;

/// Encoded bytes for a worst-case transaction: a token plus a 68-byte data
/// packet, each with up to 1/6 stuffing overhead, EOP/gap/release trailers,
/// 4 symbols per byte.
const TX_ENCODED_BYTES: usize = 176;

/// NRZI-encode `packet` (sync byte included) plus bit stuffing into 2-bit PIO
/// jump symbols, packed 4 per byte MSB-first — the same layout Pico-PIO-USB
/// feeds by 8-bit DMA. The state machine shifts LEFT with an autopull
/// threshold of 8, which is the reference project's proven combination for
/// `out pc` + autopull. Returns the number of bytes used. `j`/`k` swap for
/// low-speed, whose idle polarity is inverted.
struct SymbolStream {
    bytes: [u8; TX_ENCODED_BYTES],
    count: usize,
}

impl SymbolStream {
    const fn new() -> Self {
        Self {
            bytes: [0; TX_ENCODED_BYTES],
            count: 0,
        }
    }

    #[inline(always)]
    fn push(&mut self, symbol: u8) {
        self.bytes[self.count / 4] |= symbol << (6 - 2 * (self.count % 4));
        self.count += 1;
    }

    /// NRZI-encode one packet (sync byte included) plus bit stuffing. NRZI
    /// level and the stuffing counter restart at every packet boundary.
    #[inline(always)]
    fn push_packet(&mut self, packet: &[u8], j: u8, k: u8) {
        let mut level_j = true;
        let mut ones = 0u8;
        for &byte in packet {
            for bit in 0..8 {
                if byte >> bit & 1 == 1 {
                    ones += 1;
                } else {
                    level_j = !level_j;
                    ones = 0;
                }
                self.push(if level_j { j } else { k });
                if ones == 6 {
                    level_j = !level_j;
                    ones = 0;
                    self.push(if level_j { j } else { k });
                }
            }
        }
        // EOP: two bit times of SE0, one driven J from address 1's
        // fall-through (which consumes the symbol after this one).
        self.push(TX_SYMBOL_EOP);
    }

    /// Terminate the stream: release the bus and pad the final byte with the
    /// harmless release instruction so leftovers cannot re-drive the bus.
    #[inline(always)]
    fn finish(&mut self) -> usize {
        self.push(TX_SYMBOL_RELEASE);
        while self.count % 4 != 0 {
            self.push(TX_SYMBOL_RELEASE);
        }
        self.count / 4
    }
}

#[inline(always)]
fn encode_tx_symbols(packet: &[u8], j: u8, k: u8, bytes: &mut [u8; TX_ENCODED_BYTES]) -> usize {
    let mut stream = SymbolStream::new();
    stream.push_packet(packet, j, k);
    let byte_count = stream.finish();
    *bytes = stream.bytes;
    byte_count
}

/// Encode a token + data transaction as one continuous transmission. The
/// token's EOP fall-through consumes the first idle symbol; two more driven-J
/// symbols make a deterministic three-bit-time inter-packet gap, immune to
/// CPU scheduling. Devices allow as little as 2 and expect at most ~7.5 bit
/// times here — a software-sequenced gap of separate sends is easily 12-24.
#[inline(always)]
fn encode_tx_transaction(
    token: &[u8],
    data: &[u8],
    j: u8,
    k: u8,
    bytes: &mut [u8; TX_ENCODED_BYTES],
) -> usize {
    let mut stream = SymbolStream::new();
    stream.push_packet(token, j, k);
    stream.push(j);
    stream.push(j);
    stream.push_packet(data, j, k);
    let byte_count = stream.finish();
    *bytes = stream.bytes;
    byte_count
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BusSpeed {
    Full,
    Low,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionError {
    Timeout,
    Malformed,
    Stall,
    BufferTooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InResult {
    Data { length: usize, pid: UsbPid },
    Nak,
}

pub struct PioUsbHost {
    tx_pio: PIO<PIO0>,
    tx_sm: StateMachine<PIO0SM1, Running>,
    tx: Tx<PIO0SM1>,
    rx_pio: PIO<PIO1>,
    rx_sm: StateMachine<PIO1SM0, Running>,
    rx: Rx<PIO1SM0>,
    edge_sm: StateMachine<PIO1SM1, Running>,
    edge_eop_address: u8,
    decoder_start_address: u8,
    speed: BusSpeed,
    last_rx: [u8; 68],
    last_rx_length: u8,
    last_rx_kind: u8,
    last_parse_error: u8,
    last_irq_flags: u8,
    last_rx_pc: u8,
    last_edge_pc: u8,
    reset_diagnostic: [u8; 7],
    tx_diagnostic: [u8; 5],
    // The device allows only ~16 bit times (1.4 µs at full speed) between its
    // DATA packet and our ACK, far too tight to run the symbol encoder. The
    // encoded ACK is cached per bus speed instead.
    ack_bytes: [u8; 8],
    ack_byte_count: usize,
}

impl PioUsbHost {
    #[allow(clippy::too_many_arguments)] // Concrete ownership tokens for three PIO state machines.
    pub fn new(
        tx_pio: PIO<PIO0>,
        tx_sm: StateMachine<PIO0SM1, Running>,
        tx: Tx<PIO0SM1>,
        rx_pio: PIO<PIO1>,
        rx_sm: StateMachine<PIO1SM0, Running>,
        rx: Rx<PIO1SM0>,
        edge_sm: StateMachine<PIO1SM1, Running>,
        edge_eop_address: u8,
        decoder_start_address: u8,
    ) -> Self {
        let mut host = Self {
            tx_pio,
            tx_sm,
            tx,
            rx_pio,
            rx_sm,
            rx,
            edge_sm,
            edge_eop_address,
            decoder_start_address,
            speed: BusSpeed::Full,
            last_rx: [0; 68],
            last_rx_length: 0,
            last_rx_kind: 0,
            last_parse_error: 0,
            last_irq_flags: 0,
            last_rx_pc: 0,
            last_edge_pc: 0,
            reset_diagnostic: [0; 7],
            tx_diagnostic: [0; 5],
            ack_bytes: [0; 8],
            ack_byte_count: 0,
        };
        host.cache_ack();
        host
    }

    fn cache_ack(&mut self) {
        let ack = [0x80, UsbPid::Ack.byte()];
        let (j, k) = self.symbol_levels();
        let mut encoded = [0u8; TX_ENCODED_BYTES];
        let count = encode_tx_symbols(&ack, j, k, &mut encoded).min(self.ack_bytes.len());
        self.ack_bytes[..count].copy_from_slice(&encoded[..count]);
        self.ack_byte_count = count;
    }

    const fn symbol_levels(&self) -> (u8, u8) {
        match self.speed {
            BusSpeed::Full => (TX_SYMBOL_DRIVE_J, TX_SYMBOL_DRIVE_K),
            BusSpeed::Low => (TX_SYMBOL_DRIVE_K, TX_SYMBOL_DRIVE_J),
        }
    }

    pub fn clear_rx_diagnostic(&mut self) {
        self.last_rx_length = 0;
        self.last_rx_kind = 0;
        self.last_parse_error = 0;
    }

    /// Return the live decoder JMP pin, edge JMP pin, and edge IN base.
    pub fn pio_pin_configuration(&self) -> [u8; 3] {
        unsafe {
            let pio = &*PIO1::ptr();
            [
                pio.sm(0).sm_execctrl().read().jmp_pin().bits(),
                pio.sm(1).sm_execctrl().read().jmp_pin().bits(),
                pio.sm(1).sm_pinctrl().read().in_base().bits(),
            ]
        }
    }

    pub const fn reset_diagnostic(&self) -> [u8; 7] {
        self.reset_diagnostic
    }

    pub const fn tx_diagnostic(&self) -> [u8; 5] {
        self.tx_diagnostic
    }

    /// Encode the most recent received packet as kind, parse error, raw length,
    /// IRQ flags, decoder PC, edge-detector PC, then raw bytes.
    pub fn rx_diagnostic(&self, output: &mut [u8]) -> usize {
        if output.len() < 6 {
            return 0;
        }
        output[0] = self.last_rx_kind;
        output[1] = self.last_parse_error;
        output[2] = self.last_rx_length;
        output[3] = self.last_irq_flags;
        output[4] = self.last_rx_pc;
        output[5] = self.last_edge_pc;
        let length = usize::from(self.last_rx_length).min(output.len() - 6);
        output[6..6 + length].copy_from_slice(&self.last_rx[..length]);
        length + 6
    }

    /// Select the root device speed discovered from its D+/D- pull-up.
    pub fn configure_speed(&mut self, speed: BusSpeed) {
        self.speed = speed;
        match speed {
            BusSpeed::Full => {
                self.tx_sm.clock_divisor_fixed_point(3, 0);
                self.rx_sm.clock_divisor_fixed_point(1, 0);
                self.edge_sm.clock_divisor_fixed_point(1, 128);
                self.configure_rx_pins(12, 13, 12);
            }
            BusSpeed::Low => {
                self.tx_sm.clock_divisor_fixed_point(24, 0);
                self.rx_sm.clock_divisor_fixed_point(1, 0);
                self.edge_sm.clock_divisor_fixed_point(12, 0);
                self.configure_rx_pins(13, 12, 13);
            }
        }
        self.cache_ack();
        let _ = self.hold_receiver();
    }

    /// Put the bus in SE0 for 100 ms, release it, and allow 10 ms recovery.
    ///
    /// The longer reset matches Pico-PIO-USB's root-port connect sequence and
    /// gives wireless receivers plenty of time to reset their internal MCU.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn reset_bus<D: TimerDevice>(&mut self, timer: &mut Timer<D>) {
        self.reset_diagnostic[0] = sample_inverted_bus_pins();
        // The TX program normally waits on IRQ0 after EOP. Stop SM1 so these
        // forced pin-control instructions cannot be held behind that wait.
        self.set_tx_enabled(false);
        self.tx_sm.clear_fifos();
        self.tx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::SET {
                destination: SetDestination::PINDIRS,
                data: 3,
            },
            delay: 0,
            side_set: Some(0),
        });
        embedded_hal::delay::DelayNs::delay_us(timer, 10);
        self.reset_diagnostic[1] = sample_inverted_bus_pins();
        embedded_hal::delay::DelayNs::delay_ms(timer, 100);
        self.reset_diagnostic[2] = sample_inverted_bus_pins();
        self.tx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::SET {
                destination: SetDestination::PINDIRS,
                data: 0,
            },
            delay: 0,
            side_set: Some(self.j_side()),
        });
        embedded_hal::delay::DelayNs::delay_us(timer, 10);
        self.reset_diagnostic[3] = sample_inverted_bus_pins();
        self.set_tx_enabled(true);
        // A device suspends after 3 ms of idle bus, and a suspended device
        // ignores the upcoming SETUP. Keep the bus awake through the whole
        // recovery interval with SOF (full-speed) or EOP keep-alives.
        for frame in 0..10u16 {
            embedded_hal::delay::DelayNs::delay_ms(timer, 1);
            let _ = self.send_sof(frame);
        }
        self.reset_diagnostic[4] = sample_inverted_bus_pins();
        self.reset_diagnostic[5] = self.tx_sm.instruction_address() as u8;
        self.reset_diagnostic[6] = self.tx_pio.get_irq_raw();
        let _ = self.hold_receiver();
    }

    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn send_sof(&mut self, frame: u16) -> Result<(), TransactionError> {
        match self.speed {
            BusSpeed::Full => self.transmit(&t2::usb_host::build_sof_packet(frame)),
            BusSpeed::Low => self.transmit(&[]),
        }
    }

    /// Receive a known packet transmitted by our own PIO TX state machine.
    /// This separates PIO RX faults from a downstream device that does not
    /// reply. The cached ACK is used as the test pattern: data packets are
    /// already proven on the wire by the device accepting SETUP, while the
    /// ACK path only ever matters mid-transaction where it cannot be
    /// observed directly.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn loopback_test<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
    ) -> Result<(), TransactionError> {
        let ack_bytes = self.ack_bytes;
        let count = self.ack_byte_count;
        if !self.prepare_receiver() {
            return Err(TransactionError::Timeout);
        }
        self.arm_receiver();
        self.open_receiver_window();
        if !self.send_symbol_bytes(&ack_bytes[..count], 2, false, false) {
            return Err(TransactionError::Timeout);
        }

        let mut packet = [0u8; 16];
        let length = self.receive(timer, &mut packet, self.packet_timeout_us(), 4)?;
        let parsed = parse_received_packet(&packet[..length]);
        self.capture_rx(4, &packet[..length], parsed.as_ref().err().copied());
        match parsed {
            Ok(ReceivedPacket::Handshake(UsbPid::Ack)) => Ok(()),
            _ => Err(TransactionError::Malformed),
        }
    }

    /// Drive the decoder's trigger IRQ from the CPU while the edge detector is
    /// held. This checks the decoder program and RX FIFO independently.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn decoder_test(&mut self) -> Result<(), TransactionError> {
        if !self.prepare_receiver() {
            return Err(TransactionError::Timeout);
        }
        self.rx_pio.clear_irq(RX_IRQ_TRIGGER);
        self.set_decoder_enabled(true);
        for _ in 0..16 {
            self.rx_pio.force_irq(RX_IRQ_TRIGGER);
            let mut spins = 10_000u32;
            while self.rx_pio.get_irq_raw() & RX_IRQ_TRIGGER != 0 && spins != 0 {
                spins -= 1;
            }
            if spins == 0 {
                self.set_decoder_enabled(false);
                self.capture_rx(5, &[], None);
                return Err(TransactionError::Timeout);
            }
        }

        let mut bytes = [0u8; 8];
        let mut length = 0usize;
        while let Some(word) = self.rx.read() {
            if length == bytes.len() {
                break;
            }
            bytes[length] = (word >> 24) as u8;
            length += 1;
        }
        self.set_decoder_enabled(false);
        self.capture_rx(5, &bytes[..length], None);
        if length == 0 {
            Err(TransactionError::Malformed)
        } else {
            Ok(())
        }
    }

    /// Complete a standard/class control read transfer on endpoint zero.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn control_read<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        address: u8,
        setup: t2::usb_host::SetupPacket,
        max_packet_size: usize,
        output: &mut [u8],
    ) -> Result<usize, TransactionError> {
        self.setup(timer, address, &setup.to_bytes())?;
        let wanted = usize::from(setup.length).min(output.len());
        let mut actual = 0usize;
        let mut expected = UsbPid::Data1;
        let mut attempts = 0u8;
        while actual < wanted && attempts < 100 {
            let mut packet = [0u8; 64];
            // A NAK, a missed response and a corrupted response are all
            // retriable; the DATA0/1 toggle deduplicates any resends.
            match match self.input(timer, address, 0, &mut packet) {
                Err(TransactionError::Stall) => return Err(TransactionError::Stall),
                Err(_) => {
                    attempts += 1;
                    wait_us(timer, 1_000);
                    continue;
                }
                Ok(result) => result,
            } {
                InResult::Nak => {
                    attempts += 1;
                    wait_us(timer, 1_000);
                    continue;
                }
                InResult::Data { length, pid } => {
                    if pid != expected {
                        // Duplicate toggle: the device has not registered our
                        // ACK yet. Pace the retry like a real host's frame
                        // schedule instead of hammering the endpoint.
                        attempts += 1;
                        wait_us(timer, 1_000);
                        continue;
                    }
                    let copy = length.min(wanted - actual);
                    output[actual..actual + copy].copy_from_slice(&packet[..copy]);
                    actual += copy;
                    expected = if expected == UsbPid::Data1 {
                        UsbPid::Data0
                    } else {
                        UsbPid::Data1
                    };
                    if length < max_packet_size {
                        break;
                    }
                }
            }
        }
        if actual < wanted {
            return Err(TransactionError::Timeout);
        }
        // Status stage for a device-to-host control transfer is a DATA1 ZLP.
        // The device may NAK while it finishes processing; keep trying.
        let mut status_result = Err(TransactionError::Timeout);
        for _ in 0..100 {
            match self.output(timer, address, 0, UsbPid::Data1, &[]) {
                Ok(()) => {
                    status_result = Ok(());
                    break;
                }
                Err(TransactionError::Stall) => return Err(TransactionError::Stall),
                Err(error) => {
                    status_result = Err(error);
                    wait_us(timer, 1_000);
                }
            }
        }
        status_result?;
        Ok(actual)
    }

    /// Complete a no-data control write transfer on endpoint zero.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn control_write<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        address: u8,
        setup: t2::usb_host::SetupPacket,
    ) -> Result<(), TransactionError> {
        self.setup(timer, address, &setup.to_bytes())?;
        for _ in 0..100 {
            let mut status = [0u8; 1];
            match self.input(timer, address, 0, &mut status) {
                Ok(InResult::Nak) => wait_us(timer, 1_000),
                Ok(InResult::Data {
                    length: 0,
                    pid: UsbPid::Data1,
                }) => return Ok(()),
                Ok(InResult::Data { .. }) => return Err(TransactionError::Malformed),
                Err(TransactionError::Stall) => return Err(TransactionError::Stall),
                Err(_) => wait_us(timer, 1_000),
            }
        }
        Err(TransactionError::Timeout)
    }

    /// SETUP token + DATA0, returning only after the device ACKs.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn setup<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        address: u8,
        setup: &[u8; 8],
    ) -> Result<(), TransactionError> {
        let token = build_token_packet(UsbPid::Setup, address, 0);
        let mut raw = [0u8; 12];
        let raw_length = build_data_packet(UsbPid::Data0, setup, &mut raw).unwrap();

        // Token and DATA0 go out as one continuous transmission with a
        // symbol-exact three-bit gap; a software-sequenced gap can exceed
        // the device's 16-18 bit-time wait for the data stage.
        let (j, k) = self.symbol_levels();
        let mut encoded = [0u8; TX_ENCODED_BYTES];
        let byte_count = encode_tx_transaction(&token, &raw[..raw_length], j, k, &mut encoded);

        // A single missed handshake is normal on a marginal bus; real hosts
        // retry a transaction a few times before declaring the endpoint
        // dead. Repeating SETUP is safe — it resets the data toggle.
        let mut last_error = TransactionError::Timeout;
        for _ in 0..3 {
            if !self.prepare_receiver() {
                return Err(TransactionError::Timeout);
            }
            self.arm_receiver();
            if !self.send_transaction_bytes(&encoded[..byte_count], raw_length) {
                return Err(TransactionError::Timeout);
            }
            let mut response = [0u8; 8];
            match self.receive(timer, &mut response, self.handshake_timeout_us(), 1) {
                Ok(length) => {
                    let parsed = parse_received_packet(&response[..length]);
                    self.capture_rx(1, &response[..length], parsed.as_ref().err().copied());
                    match parsed {
                        Ok(ReceivedPacket::Handshake(UsbPid::Ack)) => return Ok(()),
                        Ok(ReceivedPacket::Handshake(UsbPid::Stall)) => {
                            return Err(TransactionError::Stall);
                        }
                        _ => last_error = TransactionError::Malformed,
                    }
                }
                Err(TransactionError::Stall) => return Err(TransactionError::Stall),
                Err(error) => last_error = error,
            }
            wait_us(timer, 500);
        }
        Err(last_error)
    }

    /// One IN transaction. Valid DATA packets are ACKed before returning.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn input<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        address: u8,
        endpoint: u8,
        output: &mut [u8],
    ) -> Result<InResult, TransactionError> {
        let token = build_token_packet(UsbPid::In, address, endpoint);
        if !self.prepare_receiver() {
            return Err(TransactionError::Timeout);
        }
        self.arm_receiver();
        if !self.transmit_packet(&token, false, true) {
            return Err(TransactionError::Timeout);
        }

        let mut packet = [0u8; 68];
        let length = self.receive_data_and_ack(timer, &mut packet)?;
        let parsed = parse_received_packet(&packet[..length]);
        self.capture_rx(2, &packet[..length], parsed.as_ref().err().copied());
        match parsed {
            Ok(ReceivedPacket::Data { pid, payload }) => {
                if output.len() < payload.len() {
                    return Err(TransactionError::BufferTooSmall);
                }
                output[..payload.len()].copy_from_slice(payload);
                Ok(InResult::Data {
                    length: payload.len(),
                    pid,
                })
            }
            Ok(ReceivedPacket::Handshake(UsbPid::Nak)) => Ok(InResult::Nak),
            Ok(ReceivedPacket::Handshake(UsbPid::Stall)) => Err(TransactionError::Stall),
            _ => Err(TransactionError::Malformed),
        }
    }

    /// Receive one packet and, if it is a CRC-valid DATA packet, fire the
    /// pre-encoded ACK the moment EOP is detected. The device only waits
    /// 16-18 bit times (1.5 µs at full speed) for the handshake — far less
    /// than a parse-then-respond round trip. A rolling CRC lagging two bytes
    /// makes the verdict available instantly at EOP, exactly like
    /// Pico-PIO-USB's receive_packet_and_handshake.
    #[inline(always)]
    fn receive_data_and_ack<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        packet: &mut [u8; 68],
    ) -> Result<usize, TransactionError> {
        let timeout_us = self.packet_timeout_us();
        let start = timer.get_counter_low();
        let mut length = 0usize;
        let mut crc = 0xffffu16;
        let mut crc_prev = 0xffffu16;
        let mut crc_prev2 = 0xffffu16;
        loop {
            while let Some(word) = self.rx.read() {
                if length == packet.len() {
                    self.capture_rx(2 | 0x80, &packet[..length], None);
                    let _ = self.hold_receiver();
                    return Err(TransactionError::BufferTooSmall);
                }
                let byte = (word >> 24) as u8;
                packet[length] = byte;
                if length >= 2 {
                    crc_prev2 = crc_prev;
                    crc_prev = crc;
                    crc = crc16_step(crc, byte);
                }
                length += 1;
            }
            if self.rx_pio.get_irq_raw() & RX_IRQ_EOP != 0 {
                while let Some(word) = self.rx.read() {
                    if length == packet.len() {
                        self.capture_rx(2 | 0x80, &packet[..length], None);
                        return Err(TransactionError::BufferTooSmall);
                    }
                    let byte = (word >> 24) as u8;
                    packet[length] = byte;
                    if length >= 2 {
                        crc_prev2 = crc_prev;
                        crc_prev = crc;
                        crc = crc16_step(crc, byte);
                    }
                    length += 1;
                }
                let crc_valid = length >= 4
                    && matches!(
                        UsbPid::from_byte(packet[1]),
                        Some(UsbPid::Data0 | UsbPid::Data1)
                    )
                    && u16::from_le_bytes([packet[length - 2], packet[length - 1]])
                        == crc_prev2 ^ 0xffff;
                if crc_valid {
                    // Keep the edge detector parked on its EOP IRQ while the
                    // ACK is sent.  Re-arming RX here makes it observe our own
                    // handshake, but also adds two MMIO updates and an
                    // artificial delay to the timing-critical turnaround.
                    // Pico-PIO-USB sends the cached ACK directly from this
                    // state and prepares RX only before the next transaction.
                    let ack_bytes = self.ack_bytes;
                    let count = self.ack_byte_count;
                    let _ = self.send_symbol_bytes(&ack_bytes[..count], 2, true, false);
                }
                return Ok(length);
            }
            if timer.get_counter_low().wrapping_sub(start) > timeout_us {
                self.capture_rx(2 | 0x80, &packet[..length], None);
                let _ = self.hold_receiver();
                return Err(TransactionError::Timeout);
            }
        }
    }

    /// OUT token + DATA packet, returning only after the device ACKs.
    #[unsafe(link_section = ".data.usb_host")]
    #[inline(never)]
    pub fn output<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        address: u8,
        endpoint: u8,
        pid: UsbPid,
        payload: &[u8],
    ) -> Result<(), TransactionError> {
        let token = build_token_packet(UsbPid::Out, address, endpoint);
        let mut raw = [0u8; 68];
        let raw_length =
            build_data_packet(pid, payload, &mut raw).ok_or(TransactionError::BufferTooSmall)?;

        let (j, k) = self.symbol_levels();
        let mut encoded = [0u8; TX_ENCODED_BYTES];
        let byte_count = encode_tx_transaction(&token, &raw[..raw_length], j, k, &mut encoded);

        if !self.prepare_receiver() {
            return Err(TransactionError::Timeout);
        }
        self.arm_receiver();
        if !self.send_transaction_bytes(&encoded[..byte_count], raw_length) {
            return Err(TransactionError::Timeout);
        }
        let mut response = [0u8; 8];
        let length = self.receive(timer, &mut response, self.handshake_timeout_us(), 3)?;
        let parsed = parse_received_packet(&response[..length]);
        self.capture_rx(3, &response[..length], parsed.as_ref().err().copied());
        match parsed {
            Ok(ReceivedPacket::Handshake(UsbPid::Ack)) => Ok(()),
            Ok(ReceivedPacket::Handshake(UsbPid::Nak)) => Err(TransactionError::Timeout),
            Ok(ReceivedPacket::Handshake(UsbPid::Stall)) => Err(TransactionError::Stall),
            _ => Err(TransactionError::Malformed),
        }
    }

    #[inline(always)]
    fn transmit(&mut self, packet: &[u8]) -> Result<(), TransactionError> {
        if self.transmit_packet(packet, true, false) {
            Ok(())
        } else {
            Err(TransactionError::Timeout)
        }
    }

    #[must_use]
    #[inline(always)]
    fn transmit_packet(
        &mut self,
        packet: &[u8],
        wait_for_release: bool,
        open_window_at_eop: bool,
    ) -> bool {
        let (j, k) = self.symbol_levels();
        let mut encoded = [0u8; TX_ENCODED_BYTES];
        let byte_count = encode_tx_symbols(packet, j, k, &mut encoded);
        self.send_symbol_bytes(
            &encoded[..byte_count],
            packet.len(),
            wait_for_release,
            open_window_at_eop,
        )
    }

    /// Transmit a pre-encoded token + data transaction in one go. The EOP
    /// flag fires twice — once per packet — so wait for the second one.
    ///
    /// The receiver window opens the moment the final EOP flag is seen: the
    /// device may answer as soon as two bit times (166 ns) after EOP, and
    /// even reading the diagnostic snapshot first loses that race. Releasing
    /// the edge detector during our own SE0/J tail is safe because D- stays
    /// low until the device's first K.
    #[must_use]
    #[inline(always)]
    fn send_transaction_bytes(&mut self, encoded: &[u8], packet_length: usize) -> bool {
        self.tx_pio.clear_irq(TX_IRQ_EOP);
        self.start_tx();
        let mut queued = 0usize;
        let mut stuck = false;
        while queued < encoded.len() {
            if !spin_until(|| self.tx.write_u8_replicated(encoded[queued])) {
                stuck = true;
                break;
            }
            queued += 1;
        }
        if !stuck {
            stuck = !spin_until(|| self.tx_pio.get_irq_raw() & TX_IRQ_EOP != 0);
            self.tx_pio.clear_irq(TX_IRQ_EOP);
        }
        if !stuck {
            stuck = !spin_until(|| self.tx_pio.get_irq_raw() & TX_IRQ_EOP != 0);
            self.open_receiver_window();
        }
        self.tx_diagnostic = [
            packet_length as u8,
            queued as u8,
            tx_fifo_level(),
            self.tx_pio.get_irq_raw(),
            self.tx_sm.instruction_address() as u8,
        ];
        !stuck
    }

    /// Push pre-encoded symbol bytes to the transmitter. The forced jump to
    /// `start` re-enables the pin drivers; the state machine then stalls on
    /// `out pc` (holding the driven idle state) until data arrives, and an
    /// underrun mid-packet stalls the same way instead of ending the packet.
    #[must_use]
    #[inline(always)]
    fn send_symbol_bytes(
        &mut self,
        encoded: &[u8],
        packet_length: usize,
        wait_for_release: bool,
        open_window_at_eop: bool,
    ) -> bool {
        self.tx_pio.clear_irq(TX_IRQ_EOP);
        self.start_tx();
        let mut queued = 0usize;
        let mut stuck = false;
        while queued < encoded.len() {
            if !spin_until(|| self.tx.write_u8_replicated(encoded[queued])) {
                stuck = true;
                break;
            }
            queued += 1;
        }
        if !stuck {
            stuck = !spin_until(|| self.tx_pio.get_irq_raw() & TX_IRQ_EOP != 0);
            if open_window_at_eop {
                self.open_receiver_window();
            }
        }
        if !stuck && wait_for_release {
            // Pico-PIO-USB's release condition: after the EOP flag, wait for
            // the program counter to reach the `set pindirs, 0b00` release
            // instruction (address 2) or the stall beyond it. This keeps the
            // inter-packet gap around a microsecond instead of a fixed delay.
            stuck =
                !spin_until(|| self.tx_sm.instruction_address() >= u32::from(TX_SYMBOL_RELEASE));
        }
        self.tx_diagnostic = [
            packet_length as u8,
            queued as u8,
            tx_fifo_level(),
            self.tx_pio.get_irq_raw(),
            self.tx_sm.instruction_address() as u8,
        ];
        !stuck
    }

    /// Force the TX state machine to its pin-enable instruction without
    /// calling `pio::Instruction::encode()`.  That helper lives in XIP flash;
    /// a cache miss here corrupts the handshake turnaround even though the
    /// enclosing USB host function itself is linked into SRAM.
    #[inline(always)]
    fn start_tx(&self) {
        // JMP always uses opcode/condition zero, so the low five bits are the
        // target address.  With mandatory 2-bit side-set, the line state is
        // stored in instruction bits 12:11.
        let instruction = u16::from(TX_START_ADDRESS) | (u16::from(self.j_side()) << 11);
        unsafe {
            let pio = &*PIO0::ptr();
            pio.sm(1)
                .sm_instr()
                .write(|w| w.sm0_instr().bits(instruction));
        }
    }

    #[must_use]
    #[inline(always)]
    fn prepare_receiver(&mut self) -> bool {
        self.set_decoder_enabled(false);
        self.rx_sm.clear_fifos();
        self.rx_sm.restart();
        // Mirror Pico-PIO-USB's rx_reset_instr: restart() does not reset the
        // program counter, so force the decoder back to its first
        // instruction before reinitializing OSR and x.
        self.rx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::JMP {
                condition: JmpCondition::Always,
                address: self.decoder_start_address,
            },
            delay: 0,
            side_set: None,
        });
        self.rx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::MOV {
                destination: MovDestination::OSR,
                op: MovOperation::Invert,
                source: MovSource::NULL,
            },
            delay: 0,
            side_set: None,
        });
        self.rx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::SET {
                destination: SetDestination::X,
                data: 0,
            },
            delay: 0,
            side_set: None,
        });
        while self.rx.read().is_some() {}
        self.set_decoder_enabled(false);
        self.hold_receiver()
    }

    #[inline(always)]
    fn hold_receiver(&mut self) -> bool {
        // StateMachine::restart() jumps to the program's wrap target, which
        // is the timing-critical sampling loop, not the EOP hold instruction.
        // Clear a stale EOP first, then jump explicitly to the first
        // instruction and wait for a newly asserted EOP. This guarantees that
        // the forced jump has executed before start_receiver releases the SM.
        self.rx_pio.clear_irq(RX_IRQ_EOP);
        self.edge_sm.exec_instruction(Instruction {
            operands: InstructionOperands::JMP {
                condition: JmpCondition::Always,
                address: self.edge_eop_address,
            },
            delay: 0,
            side_set: None,
        });
        spin_until(|| self.rx_pio.get_irq_raw() & RX_IRQ_EOP != 0)
    }

    #[inline(always)]
    fn arm_receiver(&self) {
        // START/TRIGGER can remain set when a timed-out transaction is forced
        // back to the EOP hold instruction. Clear them before enabling the
        // decoder, but keep EOP set so the edge detector remains held.
        self.rx_pio.clear_irq(RX_IRQ_START | RX_IRQ_TRIGGER);
        self.set_decoder_enabled(true);
    }

    #[inline(always)]
    fn open_receiver_window(&self) {
        // Pico-PIO-USB's start_receive clears every RX flag at once: EOP
        // releases the edge detector's hold, and a stale START/TRIGGER would
        // hand the decoder a phantom first bit.
        self.rx_pio
            .clear_irq(RX_IRQ_EOP | RX_IRQ_START | RX_IRQ_TRIGGER);
    }

    #[inline(always)]
    fn set_decoder_enabled(&self, enabled: bool) {
        // StateMachine::restart() necessarily re-enables a Running-typed SM.
        // This struct owns PIO1 SM0, so mirror the SDK's receive sequence by
        // controlling its enable bit directly around outgoing transactions.
        unsafe {
            let pio = &*PIO1::ptr();
            pio.ctrl().modify(|r, w| {
                let bits = if enabled { r.bits() | 1 } else { r.bits() & !1 };
                w.bits(bits)
            });
        }
    }

    #[inline(always)]
    fn set_tx_enabled(&self, enabled: bool) {
        // This host owns PIO0 SM1. Keep the HAL's Running token because the
        // hardware pause is brief and fully contained by reset_bus().
        unsafe {
            let pio = &*PIO0::ptr();
            pio.ctrl().modify(|r, w| {
                let bits = if enabled {
                    r.bits() | (1 << 1)
                } else {
                    r.bits() & !(1 << 1)
                };
                w.bits(bits)
            });
        }
    }

    fn configure_rx_pins(&mut self, decoder_jmp: u8, edge_jmp: u8, edge_in: u8) {
        // rp235x-hal 0.3 has no runtime JMP/IN pin setters. These registers
        // are safe to update here because this struct owns both PIO1 SMs.
        unsafe {
            let pio = &*PIO1::ptr();
            pio.sm(0)
                .sm_execctrl()
                .modify(|_, w| w.jmp_pin().bits(decoder_jmp));
            pio.sm(1)
                .sm_execctrl()
                .modify(|_, w| w.jmp_pin().bits(edge_jmp));
            pio.sm(1)
                .sm_pinctrl()
                .modify(|_, w| w.in_base().bits(edge_in));
        }
    }

    #[inline(always)]
    fn capture_rx(&mut self, kind: u8, packet: &[u8], error: Option<PacketError>) {
        let length = packet.len().min(self.last_rx.len());
        self.last_rx[..length].copy_from_slice(&packet[..length]);
        self.last_rx_length = length as u8;
        self.last_rx_kind = kind;
        self.last_irq_flags = self.rx_pio.get_irq_raw();
        self.last_rx_pc = self.rx_sm.instruction_address() as u8;
        self.last_edge_pc = self.edge_sm.instruction_address() as u8;
        self.last_parse_error = match error {
            None => 0,
            Some(PacketError::TooShort) => 1,
            Some(PacketError::BadSync) => 2,
            Some(PacketError::BadPid) => 3,
            Some(PacketError::BadLength) => 4,
            Some(PacketError::BadCrc5) => 5,
            Some(PacketError::BadCrc16) => 6,
        };
    }

    const fn j_side(&self) -> u8 {
        match self.speed {
            BusSpeed::Full => FS_J_SIDE,
            BusSpeed::Low => LS_J_SIDE,
        }
    }

    const fn handshake_timeout_us(&self) -> u32 {
        // Diagnostic-wide window: a spec-compliant answer arrives within
        // ~2 µs at full speed, but a late answer is indistinguishable from
        // silence with a tight window. Tighten once enumeration is solid.
        match self.speed {
            BusSpeed::Full => 100,
            BusSpeed::Low => 200,
        }
    }

    const fn packet_timeout_us(&self) -> u32 {
        match self.speed {
            BusSpeed::Full => 100,
            BusSpeed::Low => 600,
        }
    }

    #[inline(always)]
    fn receive<D: TimerDevice>(
        &mut self,
        timer: &Timer<D>,
        output: &mut [u8],
        timeout_us: u32,
        kind: u8,
    ) -> Result<usize, TransactionError> {
        let start = timer.get_counter_low();
        let mut length = 0usize;
        loop {
            while let Some(word) = self.rx.read() {
                if length == output.len() {
                    self.capture_rx(kind | 0x80, &output[..length], None);
                    let _ = self.hold_receiver();
                    return Err(TransactionError::BufferTooSmall);
                }
                output[length] = (word >> 24) as u8;
                length += 1;
            }
            if self.rx_pio.get_irq_raw() & RX_IRQ_EOP != 0 {
                self.set_decoder_enabled(false);
                while let Some(word) = self.rx.read() {
                    if length == output.len() {
                        self.capture_rx(kind | 0x80, &output[..length], None);
                        return Err(TransactionError::BufferTooSmall);
                    }
                    output[length] = (word >> 24) as u8;
                    length += 1;
                }
                return Ok(length);
            }
            if timer.get_counter_low().wrapping_sub(start) > timeout_us {
                // Preserve the real transaction state before hold_receiver()
                // forces EOP and changes both PCs/IRQ flags.
                self.capture_rx(kind | 0x80, &output[..length], None);
                let _ = self.hold_receiver();
                return Err(TransactionError::Timeout);
            }
        }
    }
}

/// Busy-wait until `done` holds, giving up after [`SPIN_LIMIT`] iterations.
#[inline(always)]
fn spin_until(mut done: impl FnMut() -> bool) -> bool {
    let mut spins = SPIN_LIMIT;
    while !done() {
        if spins == 0 {
            return false;
        }
        spins -= 1;
    }
    true
}

#[inline(always)]
fn wait_us<D: TimerDevice>(timer: &Timer<D>, microseconds: u32) {
    let start = timer.get_counter_low();
    while timer.get_counter_low().wrapping_sub(start) < microseconds {}
}

#[inline(always)]
fn sample_inverted_bus_pins() -> u8 {
    let gpio = unsafe { (*hal::pac::SIO::ptr()).gpio_in().read().bits() };
    ((gpio >> 12) & 0x03) as u8
}

#[inline(always)]
fn tx_fifo_level() -> u8 {
    unsafe { (*PIO0::ptr()).flevel().read().tx1().bits() }
}

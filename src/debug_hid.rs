//! Driver-free vendor HID telemetry for debugging the PIO USB host.

use usb_device::{
    Result as UsbResult,
    bus::{InterfaceNumber, UsbBus, UsbBusAllocator},
    class_prelude::{ControlIn, ControlOut, DescriptorWriter, EndpointIn, UsbClass},
    control::{Recipient, RequestType},
};

const DESCRIPTOR_HID: u8 = 0x21;
const DESCRIPTOR_REPORT: u8 = 0x22;
const REQUEST_GET_REPORT: u8 = 0x01;
const REQUEST_SET_REPORT: u8 = 0x09;
const REQUEST_SET_IDLE: u8 = 0x0a;
const REQUEST_SET_PROTOCOL: u8 = 0x0b;

pub const EVENT_BOOT: u8 = 1;
pub const EVENT_LINE_STATE: u8 = 2;
pub const EVENT_ENUM_START: u8 = 3;
pub const EVENT_ENUM_FAILED: u8 = 4;
pub const EVENT_ENUM_OK: u8 = 5;
pub const EVENT_HID_REPORT: u8 = 6;
pub const EVENT_TRANSACTION_ERROR: u8 = 7;
pub const EVENT_PIO_LOOPBACK: u8 = 8;
pub const EVENT_DECODER_TEST: u8 = 9;
pub const EVENT_BUS_RESET: u8 = 10;
pub const EVENT_TX_STATE: u8 = 11;
pub const EVENT_HID_DECODED: u8 = 12;

pub static DEBUG_REPORT_DESCRIPTOR: &[u8] = &[
    0x06, 0x00, 0xff, // Usage Page (Vendor 0xff00)
    0x09, 0x01, // Usage 1
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum 0
    0x26, 0xff, 0x00, // Logical Maximum 255
    0x75, 0x08, // Report Size 8
    0x95, 0x40, // Report Count 64
    0x09, 0x01, // Usage 1
    0x81, 0x02, // Input (Data, Variable, Absolute)
    0x09, 0x02, // Usage 2
    0xb1, 0x02, // Feature (Data, Variable, Absolute)
    0xc0,
];

pub struct DebugHid<'a, B: UsbBus> {
    interface: InterfaceNumber,
    endpoint: EndpointIn<'a, B>,
    reboot_request: u8,
}

impl<'a, B: UsbBus> DebugHid<'a, B> {
    pub fn new(alloc: &'a UsbBusAllocator<B>) -> Self {
        Self {
            interface: alloc.interface(),
            endpoint: alloc.interrupt(64, 10),
            reboot_request: 0,
        }
    }

    pub fn push(&self, report: &[u8; 64]) -> UsbResult<usize> {
        self.endpoint.write(report)
    }

    /// 1 requests a normal reboot; 2 requests the ROM BOOTSEL interface.
    pub fn take_reboot_request(&mut self) -> u8 {
        let request = self.reboot_request;
        self.reboot_request = 0;
        request
    }
}

impl<B: UsbBus> UsbClass<B> for DebugHid<'_, B> {
    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> UsbResult<()> {
        writer.interface(self.interface, 0x03, 0, 0)?;
        writer.write(
            DESCRIPTOR_HID,
            &[
                0x11,
                0x01,
                0x00,
                0x01,
                DESCRIPTOR_REPORT,
                DEBUG_REPORT_DESCRIPTOR.len() as u8,
                0,
            ],
        )?;
        writer.endpoint(&self.endpoint)
    }

    fn control_in(&mut self, xfer: ControlIn<B>) {
        let req = *xfer.request();
        if req.recipient != Recipient::Interface || req.index as u8 != u8::from(self.interface) {
            return;
        }
        if req.request_type == RequestType::Standard
            && req.request == 0x06
            && (req.value >> 8) as u8 == DESCRIPTOR_REPORT
        {
            let _ = xfer.accept_with_static(DEBUG_REPORT_DESCRIPTOR);
        } else if req.request_type == RequestType::Class && req.request == REQUEST_GET_REPORT {
            let _ = xfer.accept_with(&[0u8; 64]);
        }
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let req = *xfer.request();
        let data = xfer.data();
        if req.recipient == Recipient::Interface
            && req.index as u8 == u8::from(self.interface)
            && req.request_type == RequestType::Class
        {
            if req.request == REQUEST_SET_REPORT {
                let command = if data.starts_with(b"PBOOT") || data.get(1..6) == Some(b"PBOOT") {
                    2
                } else if data.starts_with(b"PRESET") || data.get(1..7) == Some(b"PRESET") {
                    1
                } else {
                    0
                };
                if command != 0 {
                    self.reboot_request = command;
                    let _ = xfer.accept();
                }
            } else if matches!(req.request, REQUEST_SET_IDLE | REQUEST_SET_PROTOCOL) {
                let _ = xfer.accept();
            }
        }
    }
}

pub struct DebugState {
    reports: [[u8; 64]; 16],
    head: usize,
    len: usize,
    sequence: u32,
}

impl DebugState {
    pub fn new() -> Self {
        Self {
            reports: [[0; 64]; 16],
            head: 0,
            len: 0,
            sequence: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        event: u8,
        timestamp_us: u32,
        speed: u8,
        stage: u8,
        error: u8,
        endpoint: u8,
        payload: &[u8],
    ) {
        self.sequence = self.sequence.wrapping_add(1);
        if self.len == self.reports.len() {
            self.head = (self.head + 1) % self.reports.len();
            self.len -= 1;
        }
        let index = (self.head + self.len) % self.reports.len();
        let report = &mut self.reports[index];
        report.fill(0);
        report[..4].copy_from_slice(b"PDBG");
        report[4] = 1;
        report[5] = event;
        report[6] = speed;
        report[7] = stage;
        report[8..12].copy_from_slice(&self.sequence.to_le_bytes());
        report[12..16].copy_from_slice(&timestamp_us.to_le_bytes());
        report[16] = error;
        report[17] = endpoint;
        let length = payload.len().min(40);
        report[18] = length as u8;
        report[20..20 + length].copy_from_slice(&payload[..length]);
        self.len += 1;
    }

    pub fn try_send<B: UsbBus>(&mut self, hid: &DebugHid<'_, B>) {
        if self.len != 0 && hid.push(&self.reports[self.head]).is_ok() {
            self.head = (self.head + 1) % self.reports.len();
            self.len -= 1;
        }
    }
}

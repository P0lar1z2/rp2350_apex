//! Minimal Xbox 360 wired-controller USB class for Windows PC validation.
//!
//! This is intentionally separate from the standards-based XInputHID class.
//! It mirrors the legacy XUSB descriptors needed by the Windows inbox Xbox
//! controller driver; console authentication is outside this class and is not
//! required when validating XInput on a Windows PC.

use usb_device::{
    Result as UsbResult, UsbDirection,
    bus::{InterfaceNumber, StringIndex, UsbBus, UsbBusAllocator},
    class_prelude::{
        DescriptorWriter, EndpointAddress, EndpointIn, EndpointOut, EndpointType, UsbClass,
    },
    descriptor::lang_id::LangID,
};

const XUSB_CLASS: u8 = 0xff;
const XUSB_SUBCLASS: u8 = 0x5d;
const XUSB_SECURITY_SUBCLASS: u8 = 0xfd;
const XUSB_DESCRIPTOR: u8 = 0x21;
const XUSB_SECURITY_DESCRIPTOR: u8 = 0x41;
const ENDPOINT_SIZE: u16 = 32;
const OUTPUT_REPORT_SIZE: usize = 32;

pub struct RuntimeXinput<'a, B: UsbBus> {
    control_interface: InterfaceNumber,
    audio_interface: InterfaceNumber,
    plugin_interface: InterfaceNumber,
    security_interface: InterfaceNumber,
    security_string: StringIndex,
    report_in: EndpointIn<'a, B>,
    report_out: EndpointOut<'a, B>,
    microphone_in: EndpointIn<'a, B>,
    audio_out: EndpointOut<'a, B>,
    unknown_in: EndpointIn<'a, B>,
    unknown_out: EndpointOut<'a, B>,
    plugin_in: EndpointIn<'a, B>,
    output_report: [u8; OUTPUT_REPORT_SIZE],
    output_report_len: u8,
}

impl<'a, B: UsbBus> RuntimeXinput<'a, B> {
    pub fn new(alloc: &'a UsbBusAllocator<B>) -> Self {
        let control_interface = alloc.interface();
        let audio_interface = alloc.interface();
        let plugin_interface = alloc.interface();
        let security_interface = alloc.interface();
        let security_string = alloc.string();

        let report_in: EndpointIn<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(1, UsbDirection::In)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                1,
            )
            .expect("XUSB report IN endpoint is available");
        let report_out: EndpointOut<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(2, UsbDirection::Out)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                8,
            )
            .expect("XUSB report OUT endpoint is available");
        let microphone_in: EndpointIn<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(3, UsbDirection::In)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                2,
            )
            .expect("XUSB microphone endpoint is available");
        let audio_out: EndpointOut<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(4, UsbDirection::Out)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                4,
            )
            .expect("XUSB audio endpoint is available");
        let unknown_in: EndpointIn<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(5, UsbDirection::In)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                0x40,
            )
            .expect("XUSB auxiliary IN endpoint is available");
        let unknown_out: EndpointOut<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(6, UsbDirection::Out)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                0x10,
            )
            .expect("XUSB auxiliary OUT endpoint is available");
        let plugin_in: EndpointIn<'a, B> = alloc
            .alloc(
                Some(EndpointAddress::from_parts(6, UsbDirection::In)),
                EndpointType::Interrupt,
                ENDPOINT_SIZE,
                0x10,
            )
            .expect("XUSB plugin endpoint is available");

        Self {
            control_interface,
            audio_interface,
            plugin_interface,
            security_interface,
            security_string,
            report_in,
            report_out,
            microphone_in,
            audio_out,
            unknown_in,
            unknown_out,
            plugin_in,
            output_report: [0; OUTPUT_REPORT_SIZE],
            output_report_len: 0,
        }
    }

    pub fn push_report(&self, report: &[u8]) -> UsbResult<usize> {
        self.report_in.write(report)
    }

    pub fn take_output_report(&mut self, destination: &mut [u8]) -> Option<usize> {
        let source_length = usize::from(self.output_report_len);
        if source_length == 0 {
            return None;
        }
        let copied = source_length.min(destination.len());
        destination[..copied].copy_from_slice(&self.output_report[..copied]);
        self.output_report_len = 0;
        Some(copied)
    }

    fn store_output_report(&mut self, report: &[u8]) {
        let length = report.len().min(self.output_report.len());
        self.output_report[..length].copy_from_slice(&report[..length]);
        self.output_report_len = length as u8;
    }

    fn discard_out(endpoint: &EndpointOut<'a, B>) {
        let mut report = [0u8; OUTPUT_REPORT_SIZE];
        let _ = endpoint.read(&mut report);
    }
}

impl<B: UsbBus> UsbClass<B> for RuntimeXinput<'_, B> {
    fn reset(&mut self) {
        self.output_report_len = 0;
    }

    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> UsbResult<()> {
        writer.interface(self.control_interface, XUSB_CLASS, XUSB_SUBCLASS, 0x01)?;
        writer.write(
            XUSB_DESCRIPTOR,
            &[
                0x00,
                0x01,
                0x01,
                0x25,
                self.report_in.address().into(),
                0x14,
                0x00,
                0x00,
                0x00,
                0x00,
                0x13,
                self.report_out.address().into(),
                0x08,
                0x00,
                0x00,
            ],
        )?;
        writer.endpoint(&self.report_in)?;
        writer.endpoint(&self.report_out)?;

        writer.interface(self.audio_interface, XUSB_CLASS, XUSB_SUBCLASS, 0x03)?;
        writer.write(
            XUSB_DESCRIPTOR,
            &[
                0x00,
                0x01,
                0x01,
                0x01,
                self.microphone_in.address().into(),
                0x40,
                0x01,
                self.audio_out.address().into(),
                0x20,
                0x16,
                self.unknown_in.address().into(),
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x16,
                self.unknown_out.address().into(),
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
            ],
        )?;
        writer.endpoint(&self.microphone_in)?;
        writer.endpoint(&self.audio_out)?;
        writer.endpoint(&self.unknown_in)?;
        writer.endpoint(&self.unknown_out)?;

        writer.interface(self.plugin_interface, XUSB_CLASS, XUSB_SUBCLASS, 0x02)?;
        writer.write(
            XUSB_DESCRIPTOR,
            &[
                0x00,
                0x01,
                0x01,
                0x22,
                self.plugin_in.address().into(),
                0x03,
                0x00,
            ],
        )?;
        writer.endpoint(&self.plugin_in)?;

        writer.interface_alt(
            self.security_interface,
            0,
            XUSB_CLASS,
            XUSB_SECURITY_SUBCLASS,
            0x13,
            Some(self.security_string),
        )?;
        writer.write(XUSB_SECURITY_DESCRIPTOR, &[0x00, 0x01, 0x01, 0x03])?;
        Ok(())
    }

    fn get_string(&self, index: StringIndex, _lang_id: LangID) -> Option<&str> {
        if index == self.security_string {
            Some("Xbox Security Method 3, Version 1.00, (C) 2005 Microsoft Corporation")
        } else {
            None
        }
    }

    fn endpoint_out(&mut self, address: EndpointAddress) {
        if address == self.report_out.address() {
            let mut report = [0u8; OUTPUT_REPORT_SIZE];
            if let Ok(length) = self.report_out.read(&mut report) {
                self.store_output_report(&report[..length]);
            }
        } else if address == self.audio_out.address() {
            Self::discard_out(&self.audio_out);
        } else if address == self.unknown_out.address() {
            Self::discard_out(&self.unknown_out);
        }
    }
}

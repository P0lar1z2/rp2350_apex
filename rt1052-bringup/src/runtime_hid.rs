//! Runtime-configured single-interface HID class for the RT1052 bridge.

use usb_device::{
    Result as UsbResult, UsbError,
    bus::StringIndex,
    bus::{InterfaceNumber, UsbBus, UsbBusAllocator},
    class_prelude::{
        ControlIn, ControlOut, DescriptorWriter, EndpointAddress, EndpointIn, EndpointOut, UsbClass,
    },
    control::{Recipient, RequestType},
    descriptor::lang_id::LangID,
};

const USB_CLASS_HID: u8 = 0x03;
const DESCRIPTOR_HID: u8 = 0x21;
const DESCRIPTOR_REPORT: u8 = 0x22;
const REQUEST_GET_REPORT: u8 = 0x01;
const REQUEST_GET_IDLE: u8 = 0x02;
const REQUEST_GET_PROTOCOL: u8 = 0x03;
const REQUEST_SET_REPORT: u8 = 0x09;
const REQUEST_SET_IDLE: u8 = 0x0a;
const REQUEST_SET_PROTOCOL: u8 = 0x0b;

pub const MAX_REPORT_DESCRIPTOR: usize = 512;
pub const MAX_HID_INTERFACES: usize = 6;
const MAX_CONTROL_REPORT: usize = 256;
const MAX_INTERRUPT_REPORT: usize = 64;

#[derive(Clone, Copy)]
pub struct HidReportProxy {
    pub get_report: fn(u8, u8, u8, &mut [u8]) -> Option<usize>,
    pub set_report: fn(u8, u8, u8, &[u8]) -> bool,
}

#[derive(Clone, Copy)]
pub struct RuntimeHidInterface {
    pub report_descriptor: &'static [u8],
    pub max_packet_size: u16,
    pub interval: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub hid_version: u16,
    pub country_code: u8,
    pub interface_string: Option<&'static str>,
    pub language_id: LangID,
}

pub struct RuntimeCompositeHid<'usb, B: UsbBus> {
    interfaces: [Option<InterfaceNumber>; MAX_HID_INTERFACES],
    interface_strings: [Option<StringIndex>; MAX_HID_INTERFACES],
    interrupt_in: [Option<EndpointIn<'usb, B>>; MAX_HID_INTERFACES],
    profiles: [Option<RuntimeHidInterface>; MAX_HID_INTERFACES],
    selected_protocol: [u8; MAX_HID_INTERFACES],
    idle: [u8; MAX_HID_INTERFACES],
    downstream_interfaces: [Option<u8>; MAX_HID_INTERFACES],
    report_proxy: Option<HidReportProxy>,
}

impl<'usb, B: UsbBus> RuntimeCompositeHid<'usb, B> {
    pub fn new(
        alloc: &'usb UsbBusAllocator<B>,
        profiles: [Option<RuntimeHidInterface>; MAX_HID_INTERFACES],
        report_proxy: Option<HidReportProxy>,
    ) -> Self {
        let interfaces = core::array::from_fn(|index| profiles[index].map(|_| alloc.interface()));
        let interface_strings = core::array::from_fn(|index| {
            profiles[index].and_then(|profile| profile.interface_string.map(|_| alloc.string()))
        });
        let interrupt_in = core::array::from_fn(|index| {
            profiles[index].map(|profile| {
                alloc.interrupt(
                    profile.max_packet_size.clamp(1, 64),
                    profile.interval.max(1),
                )
            })
        });
        Self {
            interfaces,
            interface_strings,
            interrupt_in,
            profiles,
            selected_protocol: [1; MAX_HID_INTERFACES],
            idle: [0; MAX_HID_INTERFACES],
            downstream_interfaces: core::array::from_fn(|index| {
                profiles[index].map(|_| index as u8)
            }),
            report_proxy,
        }
    }

    pub fn push_report(&self, index: usize, report: &[u8]) -> UsbResult<usize> {
        self.interrupt_in
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(UsbError::InvalidEndpoint)?
            .write(report)
    }

    pub fn set_downstream_interface(&mut self, source_index: usize, interface_index: Option<u8>) {
        if source_index < MAX_HID_INTERFACES && self.profiles[source_index].is_some() {
            self.downstream_interfaces[source_index] = interface_index;
        }
    }

    fn slot_for_interface(&self, interface: u16) -> Option<usize> {
        self.interfaces.iter().position(|candidate| {
            candidate.is_some_and(|number| u8::from(number) == interface as u8)
        })
    }
}

impl<B: UsbBus> UsbClass<B> for RuntimeCompositeHid<'_, B> {
    fn reset(&mut self) {
        self.selected_protocol.fill(1);
        self.idle.fill(0);
    }

    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> UsbResult<()> {
        for index in 0..MAX_HID_INTERFACES {
            let (Some(interface), Some(endpoint), Some(profile)) = (
                self.interfaces[index],
                self.interrupt_in[index].as_ref(),
                self.profiles[index],
            ) else {
                continue;
            };
            writer.interface_alt(
                interface,
                0,
                USB_CLASS_HID,
                profile.subclass,
                profile.protocol,
                self.interface_strings[index],
            )?;
            let descriptor_len = profile.report_descriptor.len() as u16;
            writer.write(
                DESCRIPTOR_HID,
                &[
                    profile.hid_version as u8,
                    (profile.hid_version >> 8) as u8,
                    profile.country_code,
                    0x01,
                    DESCRIPTOR_REPORT,
                    descriptor_len as u8,
                    (descriptor_len >> 8) as u8,
                ],
            )?;
            writer.endpoint(endpoint)?;
        }
        Ok(())
    }

    fn control_in(&mut self, xfer: ControlIn<B>) {
        let req = *xfer.request();
        if req.recipient != Recipient::Interface {
            return;
        }
        let Some(index) = self.slot_for_interface(req.index) else {
            return;
        };
        let Some(profile) = self.profiles[index] else {
            return;
        };
        if req.request_type == RequestType::Standard
            && req.request == 0x06
            && (req.value >> 8) as u8 == DESCRIPTOR_REPORT
        {
            /* Report descriptors can exceed usb-device's 256-byte copied
             * control buffer. They live for the firmware lifetime, so stream
             * them directly through EP0 instead of returning BufferOverflow
             * and stalling the host request. */
            let _ = xfer.accept_with_static(profile.report_descriptor);
            return;
        }
        if req.request_type != RequestType::Class {
            return;
        }
        match req.request {
            REQUEST_GET_REPORT => {
                let (Some(proxy), Some(interface_index)) =
                    (self.report_proxy, self.downstream_interfaces[index])
                else {
                    return;
                };
                let mut report = [0u8; MAX_CONTROL_REPORT];
                let requested = usize::from(req.length).min(report.len());
                let Some(length) = (proxy.get_report)(
                    interface_index,
                    req.value as u8,
                    (req.value >> 8) as u8,
                    &mut report[..requested],
                ) else {
                    return;
                };
                let _ = xfer.accept_with(&report[..length.min(requested)]);
            }
            REQUEST_GET_IDLE => {
                let _ = xfer.accept_with(core::slice::from_ref(&self.idle[index]));
            }
            REQUEST_GET_PROTOCOL if profile.subclass != 0 => {
                let _ = xfer.accept_with(core::slice::from_ref(&self.selected_protocol[index]));
            }
            _ => {}
        }
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let req = *xfer.request();
        if req.request_type != RequestType::Class || req.recipient != Recipient::Interface {
            return;
        }
        let Some(index) = self.slot_for_interface(req.index) else {
            return;
        };
        let Some(profile) = self.profiles[index] else {
            return;
        };
        match req.request {
            REQUEST_SET_REPORT => {
                let (Some(proxy), Some(interface_index)) =
                    (self.report_proxy, self.downstream_interfaces[index])
                else {
                    return;
                };
                if !(proxy.set_report)(
                    interface_index,
                    req.value as u8,
                    (req.value >> 8) as u8,
                    xfer.data(),
                ) {
                    return;
                }
            }
            REQUEST_SET_IDLE => self.idle[index] = (req.value >> 8) as u8,
            REQUEST_SET_PROTOCOL if profile.subclass != 0 => {
                self.selected_protocol[index] = req.value as u8;
            }
            _ => return,
        }
        let _ = xfer.accept();
    }

    fn get_string(&self, string_index: StringIndex, language_id: LangID) -> Option<&str> {
        let index = self
            .interface_strings
            .iter()
            .position(|candidate| *candidate == Some(string_index))?;
        let profile = self.profiles[index]?;
        (profile.language_id == language_id)
            .then_some(profile.interface_string)
            .flatten()
    }
}

/// Convert a source endpoint interval into a High-Speed bInterval.
///
/// High-Speed uses powers of two in 125 us microframes. Low- and Full-Speed
/// intervals are milliseconds, so a 1 ms source becomes bInterval=4 rather
/// than bInterval=1 (which would incorrectly advertise 8 kHz).
pub const fn high_speed_interval(source_speed: u8, source_interval: u8) -> u8 {
    if source_speed == 2 {
        return if source_interval < 1 {
            1
        } else if source_interval > 16 {
            16
        } else {
            source_interval
        };
    }

    let source_interval = if source_interval == 0 {
        1
    } else {
        source_interval
    };
    let target_microframes = source_interval as u16 * 8;
    let mut interval = 1u8;
    let mut microframes = 1u16;
    while microframes < target_microframes && interval < 16 {
        microframes <<= 1;
        interval += 1;
    }
    interval
}

pub struct RuntimeHid<'a, B: UsbBus> {
    interface: InterfaceNumber,
    interrupt_in: EndpointIn<'a, B>,
    interrupt_out: Option<EndpointOut<'a, B>>,
    report_descriptor: &'static [u8],
    report_descriptor_len: u16,
    subclass: u8,
    protocol: u8,
    selected_protocol: u8,
    idle: u8,
    input_report: [u8; MAX_INTERRUPT_REPORT],
    input_report_len: u8,
    output_report: [u8; MAX_INTERRUPT_REPORT],
    output_report_len: u8,
}

impl<'a, B: UsbBus> RuntimeHid<'a, B> {
    pub fn new(
        alloc: &'a UsbBusAllocator<B>,
        report_descriptor: &'static [u8],
        max_packet_size: u16,
        interval: u8,
        subclass: u8,
        protocol: u8,
    ) -> Self {
        assert!(report_descriptor.len() <= MAX_REPORT_DESCRIPTOR);
        Self {
            interface: alloc.interface(),
            interrupt_in: alloc.interrupt(max_packet_size, interval),
            interrupt_out: None,
            report_descriptor,
            report_descriptor_len: report_descriptor.len() as u16,
            subclass,
            protocol,
            selected_protocol: 1,
            idle: 0,
            input_report: [0; MAX_INTERRUPT_REPORT],
            input_report_len: 0,
            output_report: [0; MAX_INTERRUPT_REPORT],
            output_report_len: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_bidirectional(
        alloc: &'a UsbBusAllocator<B>,
        report_descriptor: &'static [u8],
        input_max_packet_size: u16,
        input_interval: u8,
        output_max_packet_size: u16,
        output_interval: u8,
        subclass: u8,
        protocol: u8,
    ) -> Self {
        let mut hid = Self::new(
            alloc,
            report_descriptor,
            input_max_packet_size,
            input_interval,
            subclass,
            protocol,
        );
        hid.interrupt_out = Some(alloc.interrupt(
            output_max_packet_size.clamp(1, MAX_INTERRUPT_REPORT as u16),
            output_interval.max(1),
        ));
        hid
    }

    pub fn set_input_report(&mut self, report: &[u8]) {
        let length = report.len().min(self.input_report.len());
        self.input_report[..length].copy_from_slice(&report[..length]);
        self.input_report_len = length as u8;
    }

    pub fn push_report(&mut self, report: &[u8]) -> UsbResult<usize> {
        let written = self.interrupt_in.write(report)?;
        self.set_input_report(&report[..written]);
        Ok(written)
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

    fn matches_interface(&self, index: u16) -> bool {
        index as u8 == u8::from(self.interface)
    }

    fn store_output_report(&mut self, report: &[u8]) {
        let length = report.len().min(self.output_report.len());
        self.output_report[..length].copy_from_slice(&report[..length]);
        self.output_report_len = length as u8;
    }
}

impl<B: UsbBus> UsbClass<B> for RuntimeHid<'_, B> {
    fn reset(&mut self) {
        self.selected_protocol = 1;
        self.idle = 0;
        self.output_report_len = 0;
    }

    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> UsbResult<()> {
        writer.interface(self.interface, USB_CLASS_HID, self.subclass, self.protocol)?;
        writer.write(
            DESCRIPTOR_HID,
            &[
                0x11,
                0x01,
                0x00,
                0x01,
                DESCRIPTOR_REPORT,
                self.report_descriptor_len as u8,
                (self.report_descriptor_len >> 8) as u8,
            ],
        )?;
        writer.endpoint(&self.interrupt_in)?;
        if let Some(interrupt_out) = self.interrupt_out.as_ref() {
            writer.endpoint(interrupt_out)?;
        }
        Ok(())
    }

    fn control_in(&mut self, xfer: ControlIn<B>) {
        let req = *xfer.request();
        if req.recipient != Recipient::Interface || !self.matches_interface(req.index) {
            return;
        }
        if req.request_type == RequestType::Standard
            && req.request == 0x06
            && (req.value >> 8) as u8 == DESCRIPTOR_REPORT
        {
            // XInputHID's standardized descriptor is 283 bytes, larger than
            // usb-device's copied 256-byte control buffer. The descriptor has
            // firmware-static lifetime, so stream it directly over EP0.
            let _ = xfer.accept_with_static(self.report_descriptor);
            return;
        }
        if req.request_type != RequestType::Class {
            return;
        }
        match req.request {
            REQUEST_GET_REPORT => {
                let report_type = (req.value >> 8) as u8;
                let report_id = req.value as u8;
                let length = usize::from(self.input_report_len);
                if report_type == 1
                    && length != 0
                    && (report_id == 0 || self.input_report[0] == report_id)
                {
                    let _ = xfer.accept_with(&self.input_report[..length]);
                }
            }
            REQUEST_GET_IDLE => {
                let _ = xfer.accept_with(core::slice::from_ref(&self.idle));
            }
            REQUEST_GET_PROTOCOL if self.subclass != 0 => {
                let _ = xfer.accept_with(core::slice::from_ref(&self.selected_protocol));
            }
            _ => {}
        }
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let req = *xfer.request();
        if req.request_type != RequestType::Class
            || req.recipient != Recipient::Interface
            || !self.matches_interface(req.index)
        {
            return;
        }
        match req.request {
            REQUEST_SET_REPORT => {
                let report_type = (req.value >> 8) as u8;
                let report_id = req.value as u8;
                let data = xfer.data();
                if report_type != 2 || (report_id != 0 && data.first().copied() != Some(report_id))
                {
                    return;
                }
                self.store_output_report(data);
            }
            REQUEST_SET_IDLE => self.idle = (req.value >> 8) as u8,
            REQUEST_SET_PROTOCOL if self.subclass != 0 => {
                self.selected_protocol = req.value as u8;
            }
            _ => return,
        }
        let _ = xfer.accept();
    }

    fn endpoint_out(&mut self, address: EndpointAddress) {
        let mut report = [0u8; MAX_INTERRUPT_REPORT];
        let result = {
            let Some(endpoint) = self.interrupt_out.as_ref() else {
                return;
            };
            if endpoint.address() != address {
                return;
            }
            endpoint.read(&mut report)
        };
        if let Ok(length) = result {
            self.store_output_report(&report[..length]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::high_speed_interval;

    #[test]
    fn preserves_high_speed_interval() {
        assert_eq!(high_speed_interval(2, 1), 1);
        assert_eq!(high_speed_interval(2, 4), 4);
    }

    #[test]
    fn translates_full_speed_milliseconds_to_microframes() {
        assert_eq!(high_speed_interval(0, 1), 4);
        assert_eq!(high_speed_interval(0, 2), 5);
        assert_eq!(high_speed_interval(1, 10), 8);
    }
}

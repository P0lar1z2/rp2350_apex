//! Runtime-configured single-interface HID class for the RT1052 bridge.

use usb_device::{
    Result as UsbResult, UsbError,
    bus::{InterfaceNumber, UsbBus, UsbBusAllocator},
    class_prelude::{ControlIn, ControlOut, DescriptorWriter, EndpointIn, UsbClass},
    control::{Recipient, RequestType},
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
pub const MAX_HID_INTERFACES: usize = 4;

#[derive(Clone, Copy)]
pub struct RuntimeHidInterface<'a> {
    pub report_descriptor: &'a [u8],
    pub max_packet_size: u16,
    pub interval: u8,
    pub subclass: u8,
    pub protocol: u8,
}

pub struct RuntimeCompositeHid<'usb, 'profile, B: UsbBus> {
    interfaces: [Option<InterfaceNumber>; MAX_HID_INTERFACES],
    interrupt_in: [Option<EndpointIn<'usb, B>>; MAX_HID_INTERFACES],
    profiles: [Option<RuntimeHidInterface<'profile>>; MAX_HID_INTERFACES],
    selected_protocol: [u8; MAX_HID_INTERFACES],
    idle: [u8; MAX_HID_INTERFACES],
}

impl<'usb, 'profile, B: UsbBus> RuntimeCompositeHid<'usb, 'profile, B> {
    pub fn new(
        alloc: &'usb UsbBusAllocator<B>,
        profiles: [Option<RuntimeHidInterface<'profile>>; MAX_HID_INTERFACES],
    ) -> Self {
        let interfaces = core::array::from_fn(|index| profiles[index].map(|_| alloc.interface()));
        let interrupt_in = core::array::from_fn(|index| {
            profiles[index].map(|profile| {
                alloc.interrupt(
                    profile.max_packet_size.clamp(1, 64),
                    profile.interval.clamp(1, 16),
                )
            })
        });
        Self {
            interfaces,
            interrupt_in,
            profiles,
            selected_protocol: [1; MAX_HID_INTERFACES],
            idle: [0; MAX_HID_INTERFACES],
        }
    }

    pub fn push_report(&self, index: usize, report: &[u8]) -> UsbResult<usize> {
        self.interrupt_in
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(UsbError::InvalidEndpoint)?
            .write(report)
    }

    fn slot_for_interface(&self, interface: u16) -> Option<usize> {
        self.interfaces.iter().position(|candidate| {
            candidate.is_some_and(|number| u8::from(number) == interface as u8)
        })
    }
}

impl<B: UsbBus> UsbClass<B> for RuntimeCompositeHid<'_, '_, B> {
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
            writer.interface(interface, USB_CLASS_HID, profile.subclass, profile.protocol)?;
            let descriptor_len = profile.report_descriptor.len() as u16;
            writer.write(
                DESCRIPTOR_HID,
                &[
                    0x11,
                    0x01,
                    0x00,
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
            let _ = xfer.accept_with(profile.report_descriptor);
            return;
        }
        if req.request_type != RequestType::Class {
            return;
        }
        match req.request {
            REQUEST_GET_REPORT => {
                let _ = xfer.accept_with(&[]);
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
            REQUEST_SET_REPORT => {}
            REQUEST_SET_IDLE => self.idle[index] = (req.value >> 8) as u8,
            REQUEST_SET_PROTOCOL if profile.subclass != 0 => {
                self.selected_protocol[index] = req.value as u8;
            }
            _ => return,
        }
        let _ = xfer.accept();
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
    report_descriptor: [u8; MAX_REPORT_DESCRIPTOR],
    report_descriptor_len: u16,
    subclass: u8,
    protocol: u8,
    selected_protocol: u8,
    idle: u8,
}

impl<'a, B: UsbBus> RuntimeHid<'a, B> {
    pub fn new(
        alloc: &'a UsbBusAllocator<B>,
        report_descriptor: &[u8],
        max_packet_size: u16,
        interval: u8,
        subclass: u8,
        protocol: u8,
    ) -> Self {
        assert!(report_descriptor.len() <= MAX_REPORT_DESCRIPTOR);
        let mut owned_descriptor = [0u8; MAX_REPORT_DESCRIPTOR];
        owned_descriptor[..report_descriptor.len()].copy_from_slice(report_descriptor);
        Self {
            interface: alloc.interface(),
            interrupt_in: alloc.interrupt(max_packet_size, interval),
            report_descriptor: owned_descriptor,
            report_descriptor_len: report_descriptor.len() as u16,
            subclass,
            protocol,
            selected_protocol: 1,
            idle: 0,
        }
    }

    pub fn push_report(&self, report: &[u8]) -> UsbResult<usize> {
        self.interrupt_in.write(report)
    }

    fn matches_interface(&self, index: u16) -> bool {
        index as u8 == u8::from(self.interface)
    }
}

impl<B: UsbBus> UsbClass<B> for RuntimeHid<'_, B> {
    fn reset(&mut self) {
        self.selected_protocol = 1;
        self.idle = 0;
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
        writer.endpoint(&self.interrupt_in)
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
            let length = usize::from(self.report_descriptor_len);
            let _ = xfer.accept_with(&self.report_descriptor[..length]);
            return;
        }
        if req.request_type != RequestType::Class {
            return;
        }
        match req.request {
            REQUEST_GET_REPORT => {
                let _ = xfer.accept_with(&[]);
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
            REQUEST_SET_IDLE => self.idle = (req.value >> 8) as u8,
            REQUEST_SET_PROTOCOL if self.subclass != 0 => {
                self.selected_protocol = req.value as u8;
            }
            _ => return,
        }
        let _ = xfer.accept();
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

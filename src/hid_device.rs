//! Native USB keyboard + mouse HID class for the RP2350 Type-C device port.
//!
//! This is implemented directly on `usb-device`; it does not use TinyUSB,
//! `usbd-hid`, a Pico SDK wrapper, or any C FFI.

use crate::usb_host::KeyboardState;

use usb_device::{
    Result as UsbResult,
    bus::{InterfaceNumber, UsbBus, UsbBusAllocator},
    class_prelude::{ControlIn, ControlOut, DescriptorWriter, EndpointIn, UsbClass},
    control::{Recipient, RequestType},
};

const USB_CLASS_HID: u8 = 0x03;
const HID_SUBCLASS_BOOT: u8 = 0x01;
const HID_PROTOCOL_KEYBOARD: u8 = 0x01;
const HID_PROTOCOL_MOUSE: u8 = 0x02;
const DESCRIPTOR_HID: u8 = 0x21;
const DESCRIPTOR_REPORT: u8 = 0x22;

const REQUEST_GET_REPORT: u8 = 0x01;
const REQUEST_GET_IDLE: u8 = 0x02;
const REQUEST_GET_PROTOCOL: u8 = 0x03;
const REQUEST_SET_REPORT: u8 = 0x09;
const REQUEST_SET_IDLE: u8 = 0x0a;
const REQUEST_SET_PROTOCOL: u8 = 0x0b;

pub static KEYBOARD_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, // Eight modifier bits
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    // Keyboard LED output byte.
    0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03,
    0x91, 0x01, // NKRO bitmap: keyboard usages 0x04..=0x73 (112 bits).
    0x05, 0x07, 0x19, 0x04, 0x29, 0x73, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x70, 0x81, 0x02,
    0xc0,
];

pub static MOUSE_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, // Mouse application
    0x09, 0x01, 0xa1, 0x00, // Pointer physical collection
    0x05, 0x09, 0x19, 0x01, 0x29, 0x08, // Eight buttons
    0x15, 0x00, 0x25, 0x01, 0x95, 0x08, 0x75, 0x01, 0x81, 0x02,
    // Signed 16-bit relative X/Y.
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02,
    0x81, 0x06, // Signed 8-bit vertical wheel and Consumer AC Pan (horizontal wheel).
    0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0x05, 0x0c, 0x0a, 0x38,
    0x02, 0x81, 0x06, 0xc0, 0xc0,
];

pub static CONSUMER_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, // Consumer Control application
    0x15, 0x00, 0x26, 0xff, 0x03, // Logical 0..0x03ff
    0x19, 0x00, 0x2a, 0xff, 0x03, // Usage 0..0x03ff
    0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xc0,
];

#[derive(Clone, Copy)]
enum NativeInterface {
    Keyboard,
    Mouse,
    Consumer,
}

pub struct KeyboardMouseHid<'a, B: UsbBus> {
    keyboard_interface: InterfaceNumber,
    mouse_interface: InterfaceNumber,
    consumer_interface: InterfaceNumber,
    keyboard_in: EndpointIn<'a, B>,
    mouse_in: EndpointIn<'a, B>,
    consumer_in: EndpointIn<'a, B>,
    keyboard_idle: u8,
    mouse_idle: u8,
    consumer_idle: u8,
    keyboard_protocol: u8,
    mouse_protocol: u8,
    keyboard_leds: u8,
}

impl<'a, B: UsbBus> KeyboardMouseHid<'a, B> {
    pub fn new(alloc: &'a UsbBusAllocator<B>) -> Self {
        Self {
            keyboard_interface: alloc.interface(),
            mouse_interface: alloc.interface(),
            consumer_interface: alloc.interface(),
            keyboard_in: alloc.interrupt(16, 1),
            mouse_in: alloc.interrupt(8, 1),
            consumer_in: alloc.interrupt(2, 1),
            keyboard_idle: 0,
            mouse_idle: 0,
            consumer_idle: 0,
            keyboard_protocol: 1,
            mouse_protocol: 1,
            keyboard_leds: 0,
        }
    }

    /// Send an eight-byte boot-keyboard state, converting it to NKRO while the
    /// computer has selected Report Protocol.
    pub fn push_keyboard(&self, report: &[u8; 8]) -> UsbResult<usize> {
        if self.keyboard_protocol == 0 {
            return self.keyboard_in.write(report);
        }
        let mut state = KeyboardState::empty();
        state.modifiers = report[0];
        for usage in &report[2..] {
            state.press(u16::from(*usage));
        }
        self.push_keyboard_state(&state)
    }

    pub fn push_keyboard_state(&self, state: &KeyboardState) -> UsbResult<usize> {
        if self.keyboard_protocol == 0 {
            let mut report = [0u8; 8];
            report[0] = state.modifiers;
            let mut count = 0usize;
            for (byte_index, byte) in state.keys.iter().copied().enumerate() {
                for bit in 0..8 {
                    if byte & (1 << bit) != 0 {
                        if count == 6 {
                            report[2..].fill(1); // Boot ErrorRollOver.
                            return self.keyboard_in.write(&report);
                        }
                        report[2 + count] = 0x04 + (byte_index * 8 + bit) as u8;
                        count += 1;
                    }
                }
            }
            self.keyboard_in.write(&report)
        } else {
            let mut report = [0u8; 15];
            report[0] = state.modifiers;
            report[1..].copy_from_slice(&state.keys);
            self.keyboard_in.write(&report)
        }
    }

    /// Send buttons, relative X/Y, and vertical wheel movement.
    pub fn push_mouse(&self, buttons: u8, x: i8, y: i8, wheel: i8) -> UsbResult<usize> {
        self.push_mouse_extended(buttons, i16::from(x), i16::from(y), wheel, 0)
    }

    pub fn push_mouse_extended(
        &self,
        buttons: u8,
        x: i16,
        y: i16,
        wheel: i8,
        pan: i8,
    ) -> UsbResult<usize> {
        if self.mouse_protocol == 0 {
            self.mouse_in.write(&[
                buttons & 0x07,
                x.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
                y.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8 as u8,
            ])
        } else {
            let x = x.to_le_bytes();
            let y = y.to_le_bytes();
            self.mouse_in
                .write(&[buttons, x[0], x[1], y[0], y[1], wheel as u8, pan as u8])
        }
    }

    pub fn push_consumer(&self, usage: u16) -> UsbResult<usize> {
        self.consumer_in.write(&usage.to_le_bytes())
    }

    pub const fn keyboard_leds(&self) -> u8 {
        self.keyboard_leds
    }

    fn interface_kind(&self, index: u16) -> Option<NativeInterface> {
        if index as u8 == u8::from(self.keyboard_interface) {
            Some(NativeInterface::Keyboard)
        } else if index as u8 == u8::from(self.mouse_interface) {
            Some(NativeInterface::Mouse)
        } else if index as u8 == u8::from(self.consumer_interface) {
            Some(NativeInterface::Consumer)
        } else {
            None
        }
    }
}

impl<B: UsbBus> UsbClass<B> for KeyboardMouseHid<'_, B> {
    fn reset(&mut self) {
        self.keyboard_idle = 0;
        self.mouse_idle = 0;
        self.consumer_idle = 0;
        self.keyboard_protocol = 1;
        self.mouse_protocol = 1;
        self.keyboard_leds = 0;
    }

    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> UsbResult<()> {
        writer.interface(
            self.keyboard_interface,
            USB_CLASS_HID,
            HID_SUBCLASS_BOOT,
            HID_PROTOCOL_KEYBOARD,
        )?;
        write_hid_descriptor(writer, KEYBOARD_REPORT_DESCRIPTOR.len())?;
        writer.endpoint(&self.keyboard_in)?;

        writer.interface(
            self.mouse_interface,
            USB_CLASS_HID,
            HID_SUBCLASS_BOOT,
            HID_PROTOCOL_MOUSE,
        )?;
        write_hid_descriptor(writer, MOUSE_REPORT_DESCRIPTOR.len())?;
        writer.endpoint(&self.mouse_in)?;

        writer.interface(self.consumer_interface, USB_CLASS_HID, 0, 0)?;
        write_hid_descriptor(writer, CONSUMER_REPORT_DESCRIPTOR.len())?;
        writer.endpoint(&self.consumer_in)?;
        Ok(())
    }

    fn control_in(&mut self, xfer: ControlIn<B>) {
        let req = *xfer.request();
        let Some(kind) = self.interface_kind(req.index) else {
            return;
        };
        if req.recipient != Recipient::Interface {
            return;
        }

        if req.request_type == RequestType::Standard
            && req.request == 0x06
            && (req.value >> 8) as u8 == DESCRIPTOR_REPORT
        {
            let descriptor = match kind {
                NativeInterface::Keyboard => KEYBOARD_REPORT_DESCRIPTOR,
                NativeInterface::Mouse => MOUSE_REPORT_DESCRIPTOR,
                NativeInterface::Consumer => CONSUMER_REPORT_DESCRIPTOR,
            };
            let _ = xfer.accept_with_static(descriptor);
            return;
        }

        if req.request_type != RequestType::Class {
            return;
        }
        let value = match req.request {
            REQUEST_GET_REPORT => match kind {
                NativeInterface::Keyboard if self.keyboard_protocol == 0 => &[0u8; 8][..],
                NativeInterface::Keyboard => &[0u8; 15][..],
                NativeInterface::Mouse if self.mouse_protocol == 0 => &[0u8; 3][..],
                NativeInterface::Mouse => &[0u8; 7][..],
                NativeInterface::Consumer => &[0u8; 2][..],
            },
            REQUEST_GET_IDLE => match kind {
                NativeInterface::Keyboard => core::slice::from_ref(&self.keyboard_idle),
                NativeInterface::Mouse => core::slice::from_ref(&self.mouse_idle),
                NativeInterface::Consumer => core::slice::from_ref(&self.consumer_idle),
            },
            REQUEST_GET_PROTOCOL => match kind {
                NativeInterface::Keyboard => core::slice::from_ref(&self.keyboard_protocol),
                NativeInterface::Mouse => core::slice::from_ref(&self.mouse_protocol),
                NativeInterface::Consumer => return,
            },
            _ => return,
        };
        let _ = xfer.accept_with(value);
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let req = *xfer.request();
        let Some(kind) = self.interface_kind(req.index) else {
            return;
        };
        if req.request_type != RequestType::Class || req.recipient != Recipient::Interface {
            return;
        }

        match req.request {
            REQUEST_SET_REPORT if matches!(kind, NativeInterface::Keyboard) => {
                if let Some(&leds) = xfer.data().first() {
                    self.keyboard_leds = leds;
                }
            }
            REQUEST_SET_IDLE => {
                let idle = (req.value >> 8) as u8;
                match kind {
                    NativeInterface::Keyboard => self.keyboard_idle = idle,
                    NativeInterface::Mouse => self.mouse_idle = idle,
                    NativeInterface::Consumer => self.consumer_idle = idle,
                }
            }
            REQUEST_SET_PROTOCOL => {
                let protocol = req.value as u8;
                match kind {
                    NativeInterface::Keyboard => self.keyboard_protocol = protocol,
                    NativeInterface::Mouse => self.mouse_protocol = protocol,
                    NativeInterface::Consumer => return,
                }
            }
            _ => return,
        }
        let _ = xfer.accept();
    }
}

fn write_hid_descriptor(writer: &mut DescriptorWriter, report_len: usize) -> UsbResult<()> {
    writer.write(
        DESCRIPTOR_HID,
        &[
            0x11,
            0x01, // HID 1.11
            0x00, // country code
            0x01, // one subordinate descriptor
            DESCRIPTOR_REPORT,
            report_len as u8,
            (report_len >> 8) as u8,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb_host::{DecodedReport, MouseState, parse_report_descriptor};

    #[test]
    fn fixed_mouse_layout_round_trips_normalized_report() {
        let decoder = parse_report_descriptor(MOUSE_REPORT_DESCRIPTOR);
        assert_eq!(
            decoder.decode(&[0x81, 0x34, 0x12, 0xfe, 0xff, 1, 0xff]),
            Some(DecodedReport::Mouse(MouseState {
                buttons: 0x81,
                x: 0x1234,
                y: -2,
                wheel: 1,
                pan: -1,
            }))
        );
    }

    #[test]
    fn fixed_keyboard_layout_is_nkro() {
        let decoder = parse_report_descriptor(KEYBOARD_REPORT_DESCRIPTOR);
        let mut report = [0u8; 15];
        report[0] = 0x02;
        report[1] = 0x03; // Keyboard usages 0x04 and 0x05.
        let DecodedReport::Keyboard(state) = decoder.decode(&report).unwrap() else {
            panic!("not a keyboard report");
        };
        assert_eq!(state.modifiers, 0x02);
        assert_eq!(state.keys[0], 0x03);
    }

    #[test]
    fn fixed_consumer_layout_carries_usage() {
        let decoder = parse_report_descriptor(CONSUMER_REPORT_DESCRIPTOR);
        assert_eq!(
            decoder.decode(&[0xe9, 0]),
            Some(DecodedReport::Consumer(0xe9))
        );
    }
}

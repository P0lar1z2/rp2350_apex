//! Runtime USB HID device clone for the native Type-C port.

use hal::timer::{Timer, TimerDevice};
use rp235x_hal as hal;
use t2::{
    macro_config::CONFIG as MACRO_CONFIG,
    macro_engine::{MacroEngine, MacroMouseReport},
    usb_host::{
        CloneProfile, DecodedReport, MouseState, ReportDecoder, SetupPacket, UsbPid,
        parse_report_descriptor,
    },
};
use usb_device::{
    UsbError,
    bus::{InterfaceNumber, UsbBus, UsbBusAllocator},
    class_prelude::{
        ControlIn, ControlOut, DescriptorWriter, EndpointAddress, EndpointIn, EndpointOut, UsbClass,
    },
    control::{Recipient, Request, RequestType},
};

use crate::pio_host::{InResult, PioUsbHost, TransactionError};

const MAX_INTERFACES: usize = 4;
const DESCRIPTOR_HID: u8 = 0x21;
const DESCRIPTOR_REPORT: u8 = 0x22;

pub struct DynamicHidClone<'usb, 'host, B: UsbBus, D: TimerDevice> {
    profile: &'host CloneProfile,
    interfaces: [Option<InterfaceNumber>; MAX_INTERFACES],
    in_endpoints: [Option<EndpointIn<'usb, B>>; MAX_INTERFACES],
    out_endpoints: [Option<EndpointOut<'usb, B>>; MAX_INTERFACES],
    host: &'host mut PioUsbHost,
    timer: &'host Timer<D>,
    expected_in_pid: [UsbPid; MAX_INTERFACES],
    next_poll: [u16; MAX_INTERFACES],
    pending_in: [[u8; 64]; MAX_INTERFACES],
    pending_len: [u8; MAX_INTERFACES],
    pending_ready: [bool; MAX_INTERFACES],
    decoders: [ReportDecoder; MAX_INTERFACES],
    macro_engine: MacroEngine,
    keyboard_interface: Option<usize>,
    keyboard_template: [u8; 64],
    keyboard_template_len: u8,
    mouse_interface: Option<usize>,
    mouse_template: [u8; 64],
    mouse_template_len: u8,
    out_pid: [UsbPid; MAX_INTERFACES],
    error_streak: [u8; MAX_INTERFACES],
    disconnected: bool,
    bootsel_countdown: u8,
}

impl<'usb, 'host, B: UsbBus, D: TimerDevice> DynamicHidClone<'usb, 'host, B, D> {
    pub fn new(
        alloc: &'usb UsbBusAllocator<B>,
        host: &'host mut PioUsbHost,
        timer: &'host Timer<D>,
        profile: &'host CloneProfile,
    ) -> Self {
        let interfaces =
            core::array::from_fn(|index| profile.interface(index).map(|_| alloc.interface()));
        let in_endpoints = core::array::from_fn(|index| {
            profile.interface(index).map(|interface| {
                // Synthetic wheel bursts are scheduled at 1 ms granularity.
                // Advertising a 1 ms upstream interval lets the host consume
                // each generated delta instead of coalescing several ticks.
                alloc.interrupt(interface.interrupt_in.max_packet_size.min(64), 1)
            })
        });
        let out_endpoints = core::array::from_fn(|index| {
            profile.interface(index).and_then(|interface| {
                interface.interrupt_out.map(|endpoint| {
                    alloc.interrupt(endpoint.max_packet_size.min(64), endpoint.interval)
                })
            })
        });
        let decoders = core::array::from_fn(|index| {
            profile
                .interface(index)
                .map_or_else(ReportDecoder::empty, |interface| {
                    parse_report_descriptor(interface.report_descriptor())
                })
        });
        let mut mouse_interface = None;
        let mut mouse_template = [0u8; 64];
        let mut mouse_template_len = 0u8;
        let empty_mouse = DecodedReport::Mouse(MouseState {
            buttons: 0,
            x: 0,
            y: 0,
            wheel: 0,
            pan: 0,
        });
        let empty_keyboard = DecodedReport::Keyboard(t2::usb_host::KeyboardState::empty());
        let mut keyboard_interface = None;
        let mut keyboard_template = [0u8; 64];
        let mut keyboard_template_len = 0u8;
        for (index, decoder) in decoders.iter().enumerate() {
            if keyboard_interface.is_none()
                && let Ok(length) = decoder.encode_new(&empty_keyboard, &mut keyboard_template)
            {
                keyboard_interface = Some(index);
                keyboard_template_len = length as u8;
            }
            if let Ok(length) = decoder.encode_new(&empty_mouse, &mut mouse_template) {
                mouse_interface = Some(index);
                mouse_template_len = length as u8;
                break;
            }
        }
        Self {
            profile,
            interfaces,
            in_endpoints,
            out_endpoints,
            host,
            timer,
            expected_in_pid: [UsbPid::Data0; MAX_INTERFACES],
            next_poll: [0; MAX_INTERFACES],
            pending_in: [[0; 64]; MAX_INTERFACES],
            pending_len: [0; MAX_INTERFACES],
            pending_ready: [false; MAX_INTERFACES],
            decoders,
            macro_engine: MacroEngine::new(
                &MACRO_CONFIG,
                u64::from(timer.get_counter_low()) ^ (profile.identity.vendor_id as u64) << 32,
            ),
            keyboard_interface,
            keyboard_template,
            keyboard_template_len,
            mouse_interface,
            mouse_template,
            mouse_template_len,
            out_pid: [UsbPid::Data0; MAX_INTERFACES],
            error_streak: [0; MAX_INTERFACES],
            disconnected: false,
            bootsel_countdown: 0,
        }
    }

    pub const fn disconnected(&self) -> bool {
        self.disconnected
    }

    /// Send the downstream SOF and service every cloned interrupt-IN pipe.
    pub fn tick(&mut self, frame: u16) {
        if self.bootsel_countdown != 0 {
            self.bootsel_countdown -= 1;
            if self.bootsel_countdown == 0 {
                hal::reboot::reboot(
                    hal::reboot::RebootKind::BootSel {
                        picoboot_disabled: false,
                        msd_disabled: false,
                    },
                    hal::reboot::RebootArch::Arm,
                );
            }
        }
        if self.host.send_sof(frame).is_err() {
            return;
        }
        for index in 0..self.profile.len() {
            self.flush_pending(index);
            if self.pending_ready[index] || frame != self.next_poll[index] {
                continue;
            }
            let Some(interface) = self.profile.interface(index) else {
                continue;
            };
            self.next_poll[index] =
                frame.wrapping_add(u16::from(interface.interrupt_in.interval)) & 0x07ff;
            let endpoint = interface.interrupt_in.address & 0x0f;
            match self
                .host
                .input(self.timer, 1, endpoint, &mut self.pending_in[index])
            {
                Ok(InResult::Nak) => self.error_streak[index] = 0,
                Ok(InResult::Data { length, pid }) if pid == self.expected_in_pid[index] => {
                    self.expected_in_pid[index] = toggle(pid);
                    self.error_streak[index] = 0;
                    self.pending_len[index] = length as u8;
                    self.transform_pending(index);
                    self.pending_ready[index] = true;
                    self.flush_pending(index);
                }
                Ok(InResult::Data { .. }) => self.error_streak[index] = 0,
                Err(TransactionError::Stall) => self.disconnected = true,
                Err(_) => {
                    self.error_streak[index] = self.error_streak[index].saturating_add(1);
                    if self.error_streak[index] >= 8 {
                        self.disconnected = true;
                    }
                }
            }
        }
        self.macro_engine.tick(self.timer.get_counter_low());
        self.queue_macro_keyboard_report();
        self.queue_macro_mouse_report();
    }

    fn transform_pending(&mut self, index: usize) {
        let length = usize::from(self.pending_len[index]);
        let Some(decoded) = self.decoders[index].decode(&self.pending_in[index][..length]) else {
            return;
        };
        let original = decoded;
        let mut transformed = self.macro_engine.observe(decoded);
        let mut needs_encode = transformed != original;
        match transformed {
            DecodedReport::Keyboard(state) => {
                self.keyboard_interface = Some(index);
                self.keyboard_template[..length].copy_from_slice(&self.pending_in[index][..length]);
                self.keyboard_template_len = length as u8;
                if let Some(generated) = self.macro_engine.take_keyboard_output() {
                    needs_encode |= generated != state;
                    transformed = DecodedReport::Keyboard(generated);
                }
            }
            DecodedReport::Mouse(physical) => {
                self.mouse_interface = Some(index);
                self.mouse_template[..length].copy_from_slice(&self.pending_in[index][..length]);
                self.mouse_template_len = length as u8;
                if let Some(generated) = self.macro_engine.take_mouse_output() {
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
        if needs_encode
            && self.decoders[index]
                .encode(&transformed, &mut self.pending_in[index][..length])
                .is_ok()
        {
            match transformed {
                DecodedReport::Keyboard(_) => self.keyboard_template[..length]
                    .copy_from_slice(&self.pending_in[index][..length]),
                DecodedReport::Mouse(_) => {
                    self.mouse_template[..length].copy_from_slice(&self.pending_in[index][..length])
                }
                DecodedReport::Consumer(_) => {}
            }
        }
    }

    fn queue_macro_keyboard_report(&mut self) {
        if !self.macro_engine.has_keyboard_output() {
            return;
        }
        let Some(index) = self.keyboard_interface else {
            return;
        };
        if self.pending_ready[index] {
            return;
        }
        let Some(generated) = self.macro_engine.take_keyboard_output() else {
            return;
        };
        let length = usize::from(self.keyboard_template_len);
        self.pending_in[index][..length].copy_from_slice(&self.keyboard_template[..length]);
        if self.decoders[index]
            .encode(
                &DecodedReport::Keyboard(generated),
                &mut self.pending_in[index][..length],
            )
            .is_err()
        {
            return;
        }
        self.pending_len[index] = length as u8;
        self.pending_ready[index] = true;
        self.flush_pending(index);
    }

    fn queue_macro_mouse_report(&mut self) {
        if !self.macro_engine.has_mouse_output() {
            return;
        }
        let Some(index) = self.mouse_interface else {
            return;
        };
        if self.pending_ready[index] {
            return;
        }
        let Some(generated) = self.macro_engine.take_mouse_output() else {
            return;
        };
        let length = usize::from(self.mouse_template_len);
        self.pending_in[index][..length].copy_from_slice(&self.mouse_template[..length]);
        let decoded = DecodedReport::Mouse(MouseState {
            buttons: generated.buttons,
            x: generated.x,
            y: generated.y,
            wheel: generated.wheel,
            pan: generated.pan,
        });
        if self.decoders[index]
            .encode(&decoded, &mut self.pending_in[index][..length])
            .is_err()
        {
            return;
        }
        self.pending_len[index] = length as u8;
        self.pending_ready[index] = true;
        self.flush_pending(index);
    }

    fn flush_pending(&mut self, index: usize) {
        if !self.pending_ready[index] {
            return;
        }
        let Some(endpoint) = self.in_endpoints[index].as_ref() else {
            return;
        };
        let length = usize::from(self.pending_len[index]);
        if endpoint.write(&self.pending_in[index][..length]).is_ok() {
            self.pending_ready[index] = false;
        }
    }

    fn profile_index_for_interface(&self, interface: u16) -> Option<usize> {
        self.interfaces.iter().position(|number| {
            number.is_some_and(|number| u16::from(u8::from(number)) == interface)
        })
    }

    fn profile_index_for_endpoint(&self, endpoint: u16) -> Option<(usize, bool)> {
        let endpoint = endpoint as u8;
        for index in 0..self.profile.len() {
            if self.in_endpoints[index]
                .as_ref()
                .is_some_and(|value| u8::from(value.address()) == endpoint)
            {
                return Some((index, true));
            }
            if self.out_endpoints[index]
                .as_ref()
                .is_some_and(|value| u8::from(value.address()) == endpoint)
            {
                return Some((index, false));
            }
        }
        None
    }

    fn translated_setup(&self, request: &Request) -> Option<SetupPacket> {
        let mut index = request.index;
        match request.recipient {
            Recipient::Interface => {
                let profile_index = self.profile_index_for_interface(index)?;
                index = u16::from(self.profile.interface(profile_index)?.original_number);
            }
            Recipient::Endpoint => {
                let (profile_index, input) = self.profile_index_for_endpoint(index)?;
                let interface = self.profile.interface(profile_index)?;
                index = u16::from(if input {
                    interface.interrupt_in.address
                } else {
                    interface.interrupt_out?.address
                });
            }
            _ => {}
        }
        Some(SetupPacket {
            request_type: request.direction as u8
                | (request.request_type as u8) << 5
                | request.recipient as u8,
            request: request.request,
            value: request.value,
            index,
            length: request.length,
        })
    }
}

impl<B: UsbBus, D: TimerDevice> UsbClass<B> for DynamicHidClone<'_, '_, B, D> {
    fn reset(&mut self) {
        self.pending_ready.fill(false);
    }

    fn get_configuration_descriptors(
        &self,
        writer: &mut DescriptorWriter,
    ) -> usb_device::Result<()> {
        for index in 0..self.profile.len() {
            let interface = self.profile.interface(index).unwrap();
            writer.interface(
                self.interfaces[index].unwrap(),
                0x03,
                interface.subclass,
                interface.protocol,
            )?;
            let report_len = interface.report_descriptor_len;
            writer.write(
                DESCRIPTOR_HID,
                &[
                    interface.hid_bcd as u8,
                    (interface.hid_bcd >> 8) as u8,
                    interface.country_code,
                    1,
                    DESCRIPTOR_REPORT,
                    report_len as u8,
                    (report_len >> 8) as u8,
                ],
            )?;
            writer.endpoint(self.in_endpoints[index].as_ref().unwrap())?;
            if let Some(endpoint) = self.out_endpoints[index].as_ref() {
                writer.endpoint(endpoint)?;
            }
        }
        Ok(())
    }

    fn control_in(&mut self, xfer: ControlIn<B>) {
        let request = *xfer.request();
        if request.recipient == Recipient::Interface
            && request.request_type == RequestType::Standard
            && request.request == Request::GET_DESCRIPTOR
        {
            let Some(index) = self.profile_index_for_interface(request.index) else {
                return;
            };
            let interface = self.profile.interface(index).unwrap();
            match (request.value >> 8) as u8 {
                DESCRIPTOR_REPORT => {
                    let _ = xfer.accept_with(interface.report_descriptor());
                    return;
                }
                DESCRIPTOR_HID => {
                    let length = interface.report_descriptor_len;
                    let descriptor = [
                        9,
                        DESCRIPTOR_HID,
                        interface.hid_bcd as u8,
                        (interface.hid_bcd >> 8) as u8,
                        interface.country_code,
                        1,
                        DESCRIPTOR_REPORT,
                        length as u8,
                        (length >> 8) as u8,
                    ];
                    let _ = xfer.accept_with(&descriptor);
                    return;
                }
                _ => {}
            }
        }

        if request.request_type == RequestType::Standard {
            return;
        }
        let Some(setup) = self.translated_setup(&request) else {
            return;
        };
        let ep0_size = usize::from(self.profile.identity.ep0_size);
        let host = &mut *self.host;
        let timer = self.timer;
        let _ = xfer.accept(|buffer| {
            let wanted = usize::from(setup.length).min(buffer.len());
            host.control_read(timer, 1, setup, ep0_size, &mut buffer[..wanted])
                .map_err(|_| UsbError::InvalidState)
        });
    }

    fn control_out(&mut self, xfer: ControlOut<B>) {
        let request = *xfer.request();
        // Unadvertised maintenance backdoor for the development tool. The
        // delay lets EP0 finish its status stage before entering the ROM.
        if request.request_type == RequestType::Vendor
            && request.recipient == Recipient::Device
            && request.request == 0x5a
            && request.value == 0x2350
            && request.index == 0x5546
        {
            self.bootsel_countdown = 10;
            let _ = xfer.accept();
            return;
        }
        if request.request_type == RequestType::Standard {
            return;
        }
        let Some(setup) = self.translated_setup(&request) else {
            return;
        };
        let result = if xfer.data().is_empty() {
            self.host.control_write(self.timer, 1, setup)
        } else {
            self.host.control_write_data(
                self.timer,
                1,
                setup,
                usize::from(self.profile.identity.ep0_size),
                xfer.data(),
            )
        };
        if result.is_ok() {
            let _ = xfer.accept();
        } else {
            let _ = xfer.reject();
        }
    }

    fn endpoint_out(&mut self, address: EndpointAddress) {
        let Some(index) = self.out_endpoints.iter().position(|endpoint| {
            endpoint
                .as_ref()
                .is_some_and(|endpoint| endpoint.address() == address)
        }) else {
            return;
        };
        let mut data = [0u8; 64];
        let Some(endpoint) = self.out_endpoints[index].as_ref() else {
            return;
        };
        let Ok(length) = endpoint.read(&mut data) else {
            return;
        };
        let downstream = self
            .profile
            .interface(index)
            .and_then(|interface| interface.interrupt_out)
            .unwrap();
        if self
            .host
            .output(
                self.timer,
                1,
                downstream.address & 0x0f,
                self.out_pid[index],
                &data[..length],
            )
            .is_ok()
        {
            self.out_pid[index] = toggle(self.out_pid[index]);
        }
    }
}

fn toggle(pid: UsbPid) -> UsbPid {
    if pid == UsbPid::Data0 {
        UsbPid::Data1
    } else {
        UsbPid::Data0
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

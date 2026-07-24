//! Runtime USB HID device clone for the native Type-C port.

use hal::timer::{Timer, TimerDevice};
use rp235x_hal as hal;
use t2::usb_host::{CloneProfile, SetupPacket, UsbPid};
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
                alloc.interrupt(
                    interface.interrupt_in.max_packet_size.min(64),
                    interface.interrupt_in.interval,
                )
            })
        });
        let out_endpoints = core::array::from_fn(|index| {
            profile.interface(index).and_then(|interface| {
                interface.interrupt_out.map(|endpoint| {
                    alloc.interrupt(endpoint.max_packet_size.min(64), endpoint.interval)
                })
            })
        });
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

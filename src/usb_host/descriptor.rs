//! Small, allocation-free parser for USB HID configuration descriptors.

use super::hid_report::ReportDecoder;

pub const MAX_HID_INTERFACES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidKind {
    Keyboard,
    Mouse,
    Generic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidEndpoint {
    pub kind: HidKind,
    pub interface: u8,
    pub endpoint: u8,
    pub max_packet_size: u16,
    pub interval_ms: u8,
    pub report_descriptor_len: u16,
    pub decoder: ReportDecoder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    Truncated,
    InvalidLength,
    TooManyHidInterfaces,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidConfiguration {
    pub configuration_value: u8,
    endpoints: [Option<HidEndpoint>; MAX_HID_INTERFACES],
    len: usize,
}

impl HidConfiguration {
    pub const fn empty(configuration_value: u8) -> Self {
        Self {
            configuration_value,
            endpoints: [None; MAX_HID_INTERFACES],
            len: 0,
        }
    }

    pub fn endpoints(&self) -> impl Iterator<Item = HidEndpoint> + '_ {
        self.endpoints[..self.len].iter().flatten().copied()
    }

    pub fn endpoints_mut(&mut self) -> impl Iterator<Item = &mut HidEndpoint> + '_ {
        self.endpoints[..self.len].iter_mut().flatten()
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, endpoint: HidEndpoint) -> Result<(), DescriptorError> {
        if self.len == self.endpoints.len() {
            return Err(DescriptorError::TooManyHidInterfaces);
        }
        self.endpoints[self.len] = Some(endpoint);
        self.len += 1;
        Ok(())
    }
}

/// Parse every HID interrupt-IN endpoint from one complete configuration
/// descriptor. Report layouts are filled later, after the host fetches each
/// interface's HID Report Descriptor.
pub fn parse_hid_configuration(bytes: &[u8]) -> Result<HidConfiguration, DescriptorError> {
    if bytes.len() < 9 {
        return Err(DescriptorError::Truncated);
    }
    if bytes[0] < 9 || bytes[1] != 2 {
        return Err(DescriptorError::InvalidLength);
    }
    let total = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    if total > bytes.len() {
        return Err(DescriptorError::Truncated);
    }

    let mut result = HidConfiguration::empty(bytes[5]);
    let mut current_interface = None;
    let mut current_report_descriptor_len = 0u16;
    let mut offset = 0usize;
    while offset < total {
        if total - offset < 2 {
            return Err(DescriptorError::Truncated);
        }
        let length = bytes[offset] as usize;
        let descriptor_type = bytes[offset + 1];
        if length < 2 || offset + length > total {
            return Err(DescriptorError::InvalidLength);
        }

        match descriptor_type {
            4 if length >= 9 => {
                let class = bytes[offset + 5];
                let subclass = bytes[offset + 6];
                let protocol = bytes[offset + 7];
                let kind = match (subclass, protocol) {
                    (1, 1) => HidKind::Keyboard,
                    (1, 2) => HidKind::Mouse,
                    _ => HidKind::Generic,
                };
                current_interface = (class == 3).then_some((bytes[offset + 2], kind));
                current_report_descriptor_len = 0;
            }
            0x21 if current_interface.is_some() && length >= 9 => {
                let subordinate_count = usize::from(bytes[offset + 5]);
                for subordinate in 0..subordinate_count {
                    let entry = offset + 6 + subordinate * 3;
                    if entry + 3 > offset + length {
                        break;
                    }
                    if bytes[entry] == 0x22 {
                        current_report_descriptor_len =
                            u16::from_le_bytes([bytes[entry + 1], bytes[entry + 2]]);
                        break;
                    }
                }
            }
            5 if length >= 7 => {
                if let Some((interface, kind)) = current_interface {
                    let address = bytes[offset + 2];
                    let attributes = bytes[offset + 3] & 0x03;
                    if address & 0x80 != 0 && attributes == 0x03 {
                        result.push(HidEndpoint {
                            kind,
                            interface,
                            endpoint: address & 0x0f,
                            max_packet_size: u16::from_le_bytes([
                                bytes[offset + 4],
                                bytes[offset + 5],
                            ]) & 0x07ff,
                            interval_ms: bytes[offset + 6].max(1),
                            report_descriptor_len: current_report_descriptor_len,
                            decoder: ReportDecoder::empty(),
                        })?;
                    }
                }
            }
            _ => {}
        }
        offset += length;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_keyboard_and_mouse_in_composite_configuration() {
        let descriptor = [
            9, 2, 59, 0, 2, 1, 0, 0x80, 50, // configuration
            9, 4, 0, 0, 1, 3, 1, 1, 0, // boot keyboard interface
            9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID
            7, 5, 0x81, 3, 8, 0, 1, // interrupt IN ep1
            9, 4, 1, 0, 1, 3, 1, 2, 0, // boot mouse interface
            9, 0x21, 0x11, 1, 0, 1, 0x22, 50, 0, // HID
            7, 5, 0x82, 3, 4, 0, 2, // interrupt IN ep2
        ];
        let parsed = parse_hid_configuration(&descriptor).unwrap();
        let mut endpoints = parsed.endpoints();
        assert_eq!(
            endpoints.next(),
            Some(HidEndpoint {
                kind: HidKind::Keyboard,
                interface: 0,
                endpoint: 1,
                max_packet_size: 8,
                interval_ms: 1,
                report_descriptor_len: 63,
                decoder: ReportDecoder::empty(),
            })
        );
        assert_eq!(
            endpoints.next(),
            Some(HidEndpoint {
                kind: HidKind::Mouse,
                interface: 1,
                endpoint: 2,
                max_packet_size: 4,
                interval_ms: 2,
                report_descriptor_len: 50,
                decoder: ReportDecoder::empty(),
            })
        );
        assert_eq!(endpoints.next(), None);
    }

    #[test]
    fn rejects_truncated_descriptor() {
        assert_eq!(
            parse_hid_configuration(&[9, 2, 32, 0, 1, 1, 0, 0x80, 50]),
            Err(DescriptorError::Truncated)
        );
    }

    #[test]
    fn accepts_non_boot_hid_interface() {
        let descriptor = [
            9, 2, 34, 0, 1, 1, 0, 0x80, 50, 9, 4, 2, 0, 1, 3, 0, 0, 0, 9, 0x21, 0x11, 1, 0, 1,
            0x22, 91, 0, 7, 5, 0x83, 3, 16, 0, 4,
        ];
        let parsed = parse_hid_configuration(&descriptor).unwrap();
        let endpoint = parsed.endpoints().next().unwrap();
        assert_eq!(endpoint.kind, HidKind::Generic);
        assert_eq!(endpoint.interface, 2);
        assert_eq!(endpoint.report_descriptor_len, 91);
    }
}

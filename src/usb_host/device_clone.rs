//! Allocation-free snapshot of one downstream USB HID device.

pub const MAX_CLONE_INTERFACES: usize = 4;
pub const MAX_REPORT_DESCRIPTOR_BYTES: usize = 256;
pub const MAX_CLONE_STRING_BYTES: usize = 63;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloneDescriptorError {
    Truncated,
    InvalidLength,
    TooManyInterfaces,
    MissingInterruptIn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloneString {
    bytes: [u8; MAX_CLONE_STRING_BYTES],
    len: usize,
}

impl CloneString {
    pub const fn empty() -> Self {
        Self {
            bytes: [0; MAX_CLONE_STRING_BYTES],
            len: 0,
        }
    }

    pub fn from_usb_descriptor(descriptor: &[u8]) -> Self {
        let mut result = Self::empty();
        if descriptor.len() < 2 || descriptor[1] != 3 {
            return result;
        }
        let end = usize::from(descriptor[0]).min(descriptor.len());
        let mut offset = 2usize;
        while offset + 1 < end {
            let codepoint = u32::from(u16::from_le_bytes([
                descriptor[offset],
                descriptor[offset + 1],
            ]));
            let character = char::from_u32(codepoint).unwrap_or('\u{fffd}');
            let mut encoded = [0u8; 4];
            let text = character.encode_utf8(&mut encoded).as_bytes();
            if result.len + text.len() > result.bytes.len() {
                break;
            }
            result.bytes[result.len..result.len + text.len()].copy_from_slice(text);
            result.len += text.len();
            offset += 2;
        }
        result
    }

    pub fn as_str(&self) -> Option<&str> {
        if self.len == 0 {
            None
        } else {
            core::str::from_utf8(&self.bytes[..self.len]).ok()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloneIdentity {
    pub usb_bcd: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub ep0_size: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_bcd: u16,
    pub manufacturer_index: u8,
    pub product_index: u8,
    pub serial_index: u8,
    pub manufacturer: CloneString,
    pub product: CloneString,
    pub serial: CloneString,
}

impl CloneIdentity {
    pub fn parse(descriptor: &[u8]) -> Result<Self, CloneDescriptorError> {
        if descriptor.len() < 18 {
            return Err(CloneDescriptorError::Truncated);
        }
        if descriptor[0] != 18 || descriptor[1] != 1 {
            return Err(CloneDescriptorError::InvalidLength);
        }
        Ok(Self {
            usb_bcd: u16::from_le_bytes([descriptor[2], descriptor[3]]),
            device_class: descriptor[4],
            device_subclass: descriptor[5],
            device_protocol: descriptor[6],
            ep0_size: descriptor[7],
            vendor_id: u16::from_le_bytes([descriptor[8], descriptor[9]]),
            product_id: u16::from_le_bytes([descriptor[10], descriptor[11]]),
            device_bcd: u16::from_le_bytes([descriptor[12], descriptor[13]]),
            manufacturer_index: descriptor[14],
            product_index: descriptor[15],
            serial_index: descriptor[16],
            manufacturer: CloneString::empty(),
            product: CloneString::empty(),
            serial: CloneString::empty(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloneEndpoint {
    pub address: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloneInterface {
    pub original_number: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub hid_bcd: u16,
    pub country_code: u8,
    pub report_descriptor_len: usize,
    pub report_descriptor: [u8; MAX_REPORT_DESCRIPTOR_BYTES],
    pub interrupt_in: CloneEndpoint,
    pub interrupt_out: Option<CloneEndpoint>,
}

impl CloneInterface {
    const fn empty() -> Self {
        Self {
            original_number: 0,
            subclass: 0,
            protocol: 0,
            hid_bcd: 0x0111,
            country_code: 0,
            report_descriptor_len: 0,
            report_descriptor: [0; MAX_REPORT_DESCRIPTOR_BYTES],
            interrupt_in: CloneEndpoint {
                address: 0,
                max_packet_size: 0,
                interval: 1,
            },
            interrupt_out: None,
        }
    }

    pub fn report_descriptor(&self) -> &[u8] {
        &self.report_descriptor[..self.report_descriptor_len]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CloneProfile {
    pub identity: CloneIdentity,
    pub configuration_value: u8,
    pub attributes: u8,
    pub max_power_2ma: u8,
    interfaces: [Option<CloneInterface>; MAX_CLONE_INTERFACES],
    len: usize,
}

impl CloneProfile {
    pub fn interfaces(&self) -> impl Iterator<Item = &CloneInterface> {
        self.interfaces[..self.len].iter().flatten()
    }

    pub fn interfaces_mut(&mut self) -> impl Iterator<Item = &mut CloneInterface> {
        self.interfaces[..self.len].iter_mut().flatten()
    }

    pub fn interface(&self, index: usize) -> Option<&CloneInterface> {
        self.interfaces.get(index).and_then(Option::as_ref)
    }

    pub const fn len(&self) -> usize {
        self.len
    }
}

/// Parse all alternate-setting-zero HID interfaces. Endpoint addresses are
/// retained for the downstream side; the upstream controller allocates its
/// own addresses while preserving sizes and intervals.
pub fn parse_clone_configuration(
    identity: CloneIdentity,
    bytes: &[u8],
) -> Result<CloneProfile, CloneDescriptorError> {
    if bytes.len() < 9 {
        return Err(CloneDescriptorError::Truncated);
    }
    let total = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
    if bytes[0] < 9 || bytes[1] != 2 || total > bytes.len() {
        return Err(CloneDescriptorError::InvalidLength);
    }
    let mut result = CloneProfile {
        identity,
        configuration_value: bytes[5],
        attributes: bytes[7],
        max_power_2ma: bytes[8],
        interfaces: [None; MAX_CLONE_INTERFACES],
        len: 0,
    };
    let mut current: Option<CloneInterface> = None;
    let mut offset = 9usize;
    while offset < total {
        if total - offset < 2 {
            return Err(CloneDescriptorError::Truncated);
        }
        let length = usize::from(bytes[offset]);
        let descriptor_type = bytes[offset + 1];
        if length < 2 || offset + length > total {
            return Err(CloneDescriptorError::InvalidLength);
        }
        if descriptor_type == 4 {
            if let Some(interface) = current.take() {
                push_interface(&mut result, interface)?;
            }
            if length >= 9
                && bytes[offset + 3] == 0
                && bytes[offset + 5] == 3
                && result.len < MAX_CLONE_INTERFACES
            {
                let mut interface = CloneInterface::empty();
                interface.original_number = bytes[offset + 2];
                interface.subclass = bytes[offset + 6];
                interface.protocol = bytes[offset + 7];
                current = Some(interface);
            }
        } else if let Some(interface) = current.as_mut() {
            match descriptor_type {
                0x21 if length >= 9 => {
                    interface.hid_bcd = u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]]);
                    interface.country_code = bytes[offset + 4];
                    for child in 0..usize::from(bytes[offset + 5]) {
                        let entry = offset + 6 + child * 3;
                        if entry + 3 <= offset + length && bytes[entry] == 0x22 {
                            interface.report_descriptor_len = usize::from(u16::from_le_bytes([
                                bytes[entry + 1],
                                bytes[entry + 2],
                            ]));
                        }
                    }
                }
                5 if length >= 7 && bytes[offset + 3] & 0x03 == 3 => {
                    let endpoint = CloneEndpoint {
                        address: bytes[offset + 2],
                        max_packet_size: u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]])
                            & 0x07ff,
                        interval: bytes[offset + 6].max(1),
                    };
                    if endpoint.address & 0x80 != 0 {
                        interface.interrupt_in = endpoint;
                    } else {
                        interface.interrupt_out = Some(endpoint);
                    }
                }
                _ => {}
            }
        }
        offset += length;
    }
    if let Some(interface) = current {
        push_interface(&mut result, interface)?;
    }
    if result.len == 0 {
        return Err(CloneDescriptorError::MissingInterruptIn);
    }
    Ok(result)
}

fn push_interface(
    profile: &mut CloneProfile,
    interface: CloneInterface,
) -> Result<(), CloneDescriptorError> {
    if interface.interrupt_in.address == 0 {
        return Ok(());
    }
    if profile.len == profile.interfaces.len() {
        return Err(CloneDescriptorError::TooManyInterfaces);
    }
    profile.interfaces[profile.len] = Some(interface);
    profile.len += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> CloneIdentity {
        CloneIdentity::parse(&[
            18, 1, 0, 2, 0, 0, 0, 64, 0x6d, 0x04, 0x2d, 0xc5, 1, 2, 1, 2, 3, 1,
        ])
        .unwrap()
    }

    #[test]
    fn parses_identity_and_utf16_string() {
        let id = identity();
        assert_eq!((id.vendor_id, id.product_id), (0x046d, 0xc52d));
        let text = CloneString::from_usb_descriptor(&[8, 3, b'L', 0, b'o', 0, b'g', 0]);
        assert_eq!(text.as_str(), Some("Log"));
    }

    #[test]
    fn retains_hid_interfaces_and_both_endpoint_directions() {
        let config = [
            9, 2, 41, 0, 1, 1, 0, 0xa0, 50, // configuration
            9, 4, 0, 0, 2, 3, 0, 0, 0, // HID interface
            9, 0x21, 0x11, 1, 0, 1, 0x22, 91, 0, // HID
            7, 5, 0x81, 3, 20, 0, 8, // IN
            7, 5, 0x02, 3, 20, 0, 8, // OUT
        ];
        let profile = parse_clone_configuration(identity(), &config).unwrap();
        let interface = profile.interfaces().next().unwrap();
        assert_eq!(profile.identity.vendor_id, 0x046d);
        assert_eq!(interface.report_descriptor_len, 91);
        assert_eq!(interface.interrupt_in.address, 0x81);
        assert_eq!(interface.interrupt_out.unwrap().address, 0x02);
    }
}

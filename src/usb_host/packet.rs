use super::crc::{crc5_token, crc16_usb};

pub const MAX_PACKET_BYTES: usize = 64 + DATA_PACKET_OVERHEAD;
pub const DATA_PACKET_OVERHEAD: usize = 4; // SYNC, PID, CRC16
pub const USB_SYNC: u8 = 0x80;

pub const PID_OUT: u8 = 0xe1;
pub const PID_IN: u8 = 0x69;
pub const PID_SOF: u8 = 0xa5;
pub const PID_SETUP: u8 = 0x2d;
pub const PID_DATA0: u8 = 0xc3;
pub const PID_DATA1: u8 = 0x4b;
pub const PID_ACK: u8 = 0xd2;
pub const PID_NAK: u8 = 0x5a;
pub const PID_STALL: u8 = 0x1e;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketError {
    TooShort,
    BadSync,
    BadPid,
    BadLength,
    BadCrc5,
    BadCrc16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceivedPacket<'a> {
    Token {
        pid: UsbPid,
        address: u8,
        endpoint: u8,
    },
    Sof {
        frame_number: u16,
    },
    Data {
        pid: UsbPid,
        payload: &'a [u8],
    },
    Handshake(UsbPid),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UsbPid {
    Out = PID_OUT,
    In = PID_IN,
    Sof = PID_SOF,
    Setup = PID_SETUP,
    Data0 = PID_DATA0,
    Data1 = PID_DATA1,
    Ack = PID_ACK,
    Nak = PID_NAK,
    Stall = PID_STALL,
}

impl UsbPid {
    pub const fn byte(self) -> u8 {
        self as u8
    }

    pub const fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            PID_OUT => Self::Out,
            PID_IN => Self::In,
            PID_SOF => Self::Sof,
            PID_SETUP => Self::Setup,
            PID_DATA0 => Self::Data0,
            PID_DATA1 => Self::Data1,
            PID_ACK => Self::Ack,
            PID_NAK => Self::Nak,
            PID_STALL => Self::Stall,
            _ => return None,
        })
    }
}

/// A valid USB PID carries a four-bit value followed by its complement.
pub const fn pid_is_valid(pid: u8) -> bool {
    ((pid >> 4) ^ (pid & 0x0f)) == 0x0f
}

/// Build an OUT, IN or SETUP token packet.
pub fn build_token_packet(pid: UsbPid, address: u8, endpoint: u8) -> [u8; 4] {
    debug_assert!(matches!(pid, UsbPid::Out | UsbPid::In | UsbPid::Setup));
    debug_assert!(address < 128);
    debug_assert!(endpoint < 16);

    let token = ((endpoint as u16) << 7) | (address as u16 & 0x7f);
    [
        USB_SYNC,
        pid.byte(),
        token as u8,
        (token >> 8) as u8 | (crc5_token(token) << 3),
    ]
}

/// Build a Start-of-Frame token. `frame_number` wraps at 2048.
pub fn build_sof_packet(frame_number: u16) -> [u8; 4] {
    let token = frame_number & 0x07ff;
    [
        USB_SYNC,
        PID_SOF,
        token as u8,
        (token >> 8) as u8 | (crc5_token(token) << 3),
    ]
}

/// Build a DATA0 or DATA1 packet in caller-owned storage.
///
/// Returns the number of initialized bytes, or `None` if the buffer is too
/// small or `pid` is not a data PID.
pub fn build_data_packet(pid: UsbPid, payload: &[u8], output: &mut [u8]) -> Option<usize> {
    if !matches!(pid, UsbPid::Data0 | UsbPid::Data1) {
        return None;
    }
    let length = payload.len().checked_add(DATA_PACKET_OVERHEAD)?;
    if output.len() < length {
        return None;
    }

    output[0] = USB_SYNC;
    output[1] = pid.byte();
    output[2..2 + payload.len()].copy_from_slice(payload);
    let crc = crc16_usb(payload).to_le_bytes();
    output[length - 2..length].copy_from_slice(&crc);
    Some(length)
}

/// Validate and classify a decoded USB packet from the PIO receiver.
pub fn parse_received_packet(bytes: &[u8]) -> Result<ReceivedPacket<'_>, PacketError> {
    if bytes.len() < 2 {
        return Err(PacketError::TooShort);
    }
    if bytes[0] != USB_SYNC {
        return Err(PacketError::BadSync);
    }
    if !pid_is_valid(bytes[1]) {
        return Err(PacketError::BadPid);
    }
    let pid = UsbPid::from_byte(bytes[1]).ok_or(PacketError::BadPid)?;

    match pid {
        UsbPid::Ack | UsbPid::Nak | UsbPid::Stall => {
            if bytes.len() != 2 {
                return Err(PacketError::BadLength);
            }
            Ok(ReceivedPacket::Handshake(pid))
        }
        UsbPid::Data0 | UsbPid::Data1 => {
            if bytes.len() < DATA_PACKET_OVERHEAD {
                return Err(PacketError::BadLength);
            }
            let payload_end = bytes.len() - 2;
            let expected = crc16_usb(&bytes[2..payload_end]);
            let received = u16::from_le_bytes([bytes[payload_end], bytes[payload_end + 1]]);
            if received != expected {
                return Err(PacketError::BadCrc16);
            }
            Ok(ReceivedPacket::Data {
                pid,
                payload: &bytes[2..payload_end],
            })
        }
        UsbPid::Out | UsbPid::In | UsbPid::Setup | UsbPid::Sof => {
            if bytes.len() != 4 {
                return Err(PacketError::BadLength);
            }
            let token = bytes[2] as u16 | ((bytes[3] as u16 & 0x07) << 8);
            if bytes[3] >> 3 != crc5_token(token) {
                return Err(PacketError::BadCrc5);
            }
            if pid == UsbPid::Sof {
                Ok(ReceivedPacket::Sof {
                    frame_number: token,
                })
            } else {
                Ok(ReceivedPacket::Token {
                    pid,
                    address: (token & 0x7f) as u8,
                    endpoint: ((token >> 7) & 0x0f) as u8,
                })
            }
        }
    }
}

/// The eight-byte payload of a USB control SETUP transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SetupPacket {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl SetupPacket {
    pub const fn to_bytes(self) -> [u8; 8] {
        let value = self.value.to_le_bytes();
        let index = self.index.to_le_bytes();
        let length = self.length.to_le_bytes();
        [
            self.request_type,
            self.request,
            value[0],
            value[1],
            index[0],
            index[1],
            length[0],
            length[1],
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_declared_pids_have_valid_complement() {
        for pid in [
            PID_OUT, PID_IN, PID_SOF, PID_SETUP, PID_DATA0, PID_DATA1, PID_ACK, PID_NAK, PID_STALL,
        ] {
            assert!(pid_is_valid(pid));
        }
        assert!(!pid_is_valid(0x00));
    }

    #[test]
    fn builds_endpoint_token() {
        let packet = build_token_packet(UsbPid::In, 5, 2);
        let token = 5 | (2 << 7);
        assert_eq!(packet[0..3], [USB_SYNC, PID_IN, token as u8]);
        assert_eq!(packet[3] >> 3, crc5_token(token));
        assert_eq!(packet[3] & 0x07, (token >> 8) as u8);
    }

    #[test]
    fn builds_setup_payload_little_endian() {
        let setup = SetupPacket {
            request_type: 0x80,
            request: 6,
            value: 0x0100,
            index: 0,
            length: 18,
        };
        assert_eq!(setup.to_bytes(), [0x80, 6, 0x00, 0x01, 0, 0, 18, 0]);
    }

    #[test]
    fn data_packet_appends_little_endian_crc() {
        let payload = b"123456789";
        let mut output = [0u8; 32];
        let len = build_data_packet(UsbPid::Data0, payload, &mut output).unwrap();
        assert_eq!(&output[..2], &[USB_SYNC, PID_DATA0]);
        assert_eq!(&output[2..2 + payload.len()], payload);
        assert_eq!(&output[len - 2..len], &[0xc8, 0xb4]);
        assert_eq!(build_data_packet(UsbPid::Ack, payload, &mut output), None);
    }

    #[test]
    fn parser_round_trips_packets_and_rejects_crc_damage() {
        let token = build_token_packet(UsbPid::In, 5, 2);
        assert_eq!(
            parse_received_packet(&token),
            Ok(ReceivedPacket::Token {
                pid: UsbPid::In,
                address: 5,
                endpoint: 2,
            })
        );

        let mut data = [0u8; 16];
        let len = build_data_packet(UsbPid::Data1, &[1, 2, 3], &mut data).unwrap();
        assert_eq!(
            parse_received_packet(&data[..len]),
            Ok(ReceivedPacket::Data {
                pid: UsbPid::Data1,
                payload: &[1, 2, 3],
            })
        );
        data[len - 1] ^= 1;
        assert_eq!(
            parse_received_packet(&data[..len]),
            Err(PacketError::BadCrc16)
        );
    }
}

//! Small, allocation-free asynchronous control protocol carried over UDP.
//!
//! Receiving a packet only validates and converts it into a `ControlCommand`.
//! Callers must enqueue the command and execute it outside ENET / USB IRQ context.

pub const MAGIC: [u8; 4] = *b"RTCP";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;
pub const MAX_PAYLOAD_LEN: usize = 16;
pub const SET_SENSITIVITY_KIND: u8 = 6;
pub const MIN_SENSITIVITY_MILLI: u16 = 100;
pub const MAX_SENSITIVITY_MILLI: u16 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlCommand {
    SetEnabled(bool),
    SelectLayer(u8),
    TriggerProgram(u8),
    ReleaseProgram(u8),
    EmergencyRelease,
    /// In-game sensitivity in thousandths. The recoil trajectory is calibrated
    /// at 1.000 and physical mouse movement is never scaled.
    SetSensitivityMilli(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandFrame {
    pub sequence: u32,
    pub command: ControlCommand,
}

/// Single-consumer bounded queue between low-priority network polling and the
/// control executor. Full queues reject the newest command and count it; they
/// never stall USB work.
pub struct ControlQueue<const N: usize> {
    entries: [Option<CommandFrame>; N],
    read: usize,
    write: usize,
    len: usize,
    dropped: u32,
}

impl<const N: usize> ControlQueue<N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            read: 0,
            write: 0,
            len: 0,
            dropped: 0,
        }
    }

    pub fn push(&mut self, command: CommandFrame) -> Result<(), CommandFrame> {
        if self.len == N {
            self.dropped = self.dropped.saturating_add(1);
            return Err(command);
        }
        self.entries[self.write] = Some(command);
        self.write = (self.write + 1) % N;
        self.len += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<CommandFrame> {
        if self.len == 0 {
            return None;
        }
        let command = self.entries[self.read].take();
        self.read = (self.read + 1) % N;
        self.len -= 1;
        command
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl<const N: usize> Default for ControlQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    UnsupportedVersion,
    BadFlags,
    BadLength,
    UnknownKind,
    BadPayload,
}

pub fn decode(packet: &[u8]) -> Result<CommandFrame, DecodeError> {
    if packet.len() < HEADER_LEN {
        return Err(DecodeError::TooShort);
    }
    if packet[..4] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    if packet[4] != VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }
    if packet[7] != 0 {
        return Err(DecodeError::BadFlags);
    }
    let payload_len = packet[6] as usize;
    if payload_len > MAX_PAYLOAD_LEN || packet.len() != HEADER_LEN + payload_len {
        return Err(DecodeError::BadLength);
    }
    let sequence = u32::from_le_bytes([packet[8], packet[9], packet[10], packet[11]]);
    let payload = &packet[HEADER_LEN..];
    let command = match (packet[5], payload) {
        (1, [enabled]) if *enabled <= 1 => ControlCommand::SetEnabled(*enabled != 0),
        (2, [layer]) => ControlCommand::SelectLayer(*layer),
        (3, [program]) => ControlCommand::TriggerProgram(*program),
        (4, [program]) => ControlCommand::ReleaseProgram(*program),
        (5, []) => ControlCommand::EmergencyRelease,
        (SET_SENSITIVITY_KIND, [low, high]) => {
            let sensitivity = u16::from_le_bytes([*low, *high]);
            if !(MIN_SENSITIVITY_MILLI..=MAX_SENSITIVITY_MILLI).contains(&sensitivity) {
                return Err(DecodeError::BadPayload);
            }
            ControlCommand::SetSensitivityMilli(sensitivity)
        }
        (1..=SET_SENSITIVITY_KIND, _) => return Err(DecodeError::BadPayload),
        _ => return Err(DecodeError::UnknownKind),
    };
    Ok(CommandFrame { sequence, command })
}

/// Encode an asynchronous acknowledgement/event datagram.
///
/// Kind `0x80` is ACK; payload is the original command kind followed by a
/// status byte. The caller may drop this datagram under backpressure.
pub fn encode_ack(
    output: &mut [u8],
    sequence: u32,
    command_kind: u8,
    status: u8,
) -> Result<usize, ()> {
    let length = HEADER_LEN + 2;
    if output.len() < length {
        return Err(());
    }
    output[..4].copy_from_slice(&MAGIC);
    output[4] = VERSION;
    output[5] = 0x80;
    output[6] = 2;
    output[7] = 0;
    output[8..12].copy_from_slice(&sequence.to_le_bytes());
    output[12] = command_kind;
    output[13] = status;
    Ok(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: u8, sequence: u32, payload: &[u8]) -> [u8; 16] {
        let mut packet = [0u8; 16];
        packet[..4].copy_from_slice(&MAGIC);
        packet[4] = VERSION;
        packet[5] = kind;
        packet[6] = payload.len() as u8;
        packet[7] = 0;
        packet[8..12].copy_from_slice(&sequence.to_le_bytes());
        packet[12..12 + payload.len()].copy_from_slice(payload);
        packet
    }

    #[test]
    fn decodes_all_initial_commands() {
        let cases = [
            (1, &[1][..], ControlCommand::SetEnabled(true)),
            (2, &[3][..], ControlCommand::SelectLayer(3)),
            (3, &[7][..], ControlCommand::TriggerProgram(7)),
            (4, &[7][..], ControlCommand::ReleaseProgram(7)),
            (5, &[][..], ControlCommand::EmergencyRelease),
            (
                SET_SENSITIVITY_KIND,
                &1_500u16.to_le_bytes()[..],
                ControlCommand::SetSensitivityMilli(1_500),
            ),
        ];
        for (kind, payload, expected) in cases {
            let packet = command(kind, 0x1234_5678, payload);
            assert_eq!(
                decode(&packet[..HEADER_LEN + payload.len()]),
                Ok(CommandFrame {
                    sequence: 0x1234_5678,
                    command: expected
                })
            );
        }
    }

    #[test]
    fn rejects_trailing_data_and_malformed_payloads() {
        let packet = command(1, 1, &[2]);
        assert_eq!(decode(&packet[..13]), Err(DecodeError::BadPayload));
        let packet = command(5, 1, &[]);
        assert_eq!(decode(&packet[..13]), Err(DecodeError::BadLength));
        let packet = command(SET_SENSITIVITY_KIND, 1, &99u16.to_le_bytes());
        assert_eq!(decode(&packet[..14]), Err(DecodeError::BadPayload));
        let packet = command(SET_SENSITIVITY_KIND, 1, &10_001u16.to_le_bytes());
        assert_eq!(decode(&packet[..14]), Err(DecodeError::BadPayload));
    }

    #[test]
    fn encodes_async_ack() {
        let mut output = [0u8; 16];
        let length = encode_ack(&mut output, 9, 3, 0).unwrap();
        assert_eq!(length, 14);
        assert_eq!(&output[..4], b"RTCP");
        assert_eq!(output[5], 0x80);
        assert_eq!(&output[8..12], &9u32.to_le_bytes());
        assert_eq!(&output[12..14], &[3, 0]);
    }

    #[test]
    fn bounded_queue_never_blocks_and_preserves_order() {
        let mut queue = ControlQueue::<2>::new();
        let first = CommandFrame {
            sequence: 1,
            command: ControlCommand::SelectLayer(2),
        };
        let second = CommandFrame {
            sequence: 2,
            command: ControlCommand::TriggerProgram(4),
        };
        let overflow = CommandFrame {
            sequence: 3,
            command: ControlCommand::EmergencyRelease,
        };
        assert_eq!(queue.push(first), Ok(()));
        assert_eq!(queue.push(second), Ok(()));
        assert_eq!(queue.push(overflow), Err(overflow));
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pop(), Some(first));
        assert_eq!(queue.pop(), Some(second));
        assert_eq!(queue.pop(), None);
    }
}

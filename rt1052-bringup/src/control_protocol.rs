//! Small, allocation-free asynchronous control protocol carried over UDP.
//!
//! Receiving a packet only validates and converts it into a `ControlCommand`.
//! Callers must enqueue the command and execute it outside ENET / USB IRQ context.

use crate::runtime_trajectory::MAX_TRAJECTORY_SLOTS;

pub const MAGIC: [u8; 4] = *b"RTCP";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;
pub const MAX_PAYLOAD_LEN: usize = 64;
pub const MAX_TRAJECTORY_CHUNK_POINTS: usize = 15;
pub const SET_SENSITIVITY_KIND: u8 = 6;
pub const SET_KEY_KIND: u8 = 7;
pub const SET_MOUSE_BUTTONS_KIND: u8 = 8;
pub const MOVE_MOUSE_KIND: u8 = 9;
pub const BEGIN_TRAJECTORY_KIND: u8 = 10;
pub const TRAJECTORY_CHUNK_KIND: u8 = 11;
pub const COMMIT_TRAJECTORY_KIND: u8 = 12;
pub const SELECT_TRAJECTORY_KIND: u8 = 13;
pub const SUBSCRIBE_INPUT_KIND: u8 = 14;
pub const QUERY_STATUS_KIND: u8 = 15;
pub const SAVE_TRAJECTORIES_KIND: u8 = 16;
pub const PROBE_FLASH_KIND: u8 = 17;
pub const QUERY_FLASH_KIND: u8 = 18;
pub const QUERY_STATUS_PAGE_KIND: u8 = 19;
pub const ACK_KIND: u8 = 0x80;
pub const INPUT_EVENT_KIND: u8 = 0x81;
pub const STATUS_EVENT_KIND: u8 = 0x82;
pub const FLASH_EVENT_KIND: u8 = 0x83;
pub const STATUS_PAGE_EVENT_KIND: u8 = 0x84;
pub const LEGACY_EXTENDED_STATUS_FLAG: u8 = 0x80;
pub const STATUS_PAGE_HEADER_LEN: usize = 6;
pub const STATUS_PAGE_SLOTS: usize = (MAX_PAYLOAD_LEN - STATUS_PAGE_HEADER_LEN) / 8;
pub const STATUS_PAGE_COUNT: usize = MAX_TRAJECTORY_SLOTS.div_ceil(STATUS_PAGE_SLOTS);
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
    /// Set one remote HID keyboard usage. `pressed=false` releases it.
    SetKey {
        usage: u8,
        pressed: bool,
    },
    /// Replace the remote mouse-button bitmask (button 1 is bit zero).
    SetMouseButtons(u8),
    /// Queue unscaled relative mouse input. This is control input, not recoil.
    MoveMouse {
        x: i16,
        y: i16,
        wheel: i8,
        pan: i8,
    },
    /// Begin a transactional upload into a staging buffer. The committed slot
    /// is unchanged until a CRC-checked commit succeeds.
    BeginTrajectory {
        slot: u8,
        length: u16,
        tick_us: u16,
        crc32: u32,
    },
    /// Sequential packed `(x: i16, y: i16)` trajectory points.
    TrajectoryChunk {
        slot: u8,
        offset: u16,
        count: u8,
        packed: [u8; MAX_TRAJECTORY_CHUNK_POINTS * 4],
    },
    CommitTrajectory(u8),
    SelectTrajectory(u8),
    SubscribeInput(bool),
    QueryStatus,
    SaveTrajectories,
    ProbeFlash(u32),
    QueryFlash,
    QueryStatusPage(u8),
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
        (SET_KEY_KIND, [usage, pressed])
            if *pressed <= 1 && matches!(*usage, 0x04..=0x73 | 0xe0..=0xe7) =>
        {
            ControlCommand::SetKey {
                usage: *usage,
                pressed: *pressed != 0,
            }
        }
        (SET_MOUSE_BUTTONS_KIND, [buttons]) => ControlCommand::SetMouseButtons(*buttons),
        (MOVE_MOUSE_KIND, [x_low, x_high, y_low, y_high, wheel, pan]) => {
            ControlCommand::MoveMouse {
                x: i16::from_le_bytes([*x_low, *x_high]),
                y: i16::from_le_bytes([*y_low, *y_high]),
                wheel: *wheel as i8,
                pan: *pan as i8,
            }
        }
        (
            BEGIN_TRAJECTORY_KIND,
            [
                slot,
                length_low,
                length_high,
                tick_low,
                tick_high,
                crc0,
                crc1,
                crc2,
                crc3,
            ],
        ) => ControlCommand::BeginTrajectory {
            slot: *slot,
            length: u16::from_le_bytes([*length_low, *length_high]),
            tick_us: u16::from_le_bytes([*tick_low, *tick_high]),
            crc32: u32::from_le_bytes([*crc0, *crc1, *crc2, *crc3]),
        },
        (TRAJECTORY_CHUNK_KIND, payload) if payload.len() >= 4 => {
            let count = usize::from(payload[3]);
            if count == 0 || count > MAX_TRAJECTORY_CHUNK_POINTS || payload.len() != 4 + count * 4 {
                return Err(DecodeError::BadPayload);
            }
            let mut packed = [0u8; MAX_TRAJECTORY_CHUNK_POINTS * 4];
            packed[..count * 4].copy_from_slice(&payload[4..]);
            ControlCommand::TrajectoryChunk {
                slot: payload[0],
                offset: u16::from_le_bytes([payload[1], payload[2]]),
                count: count as u8,
                packed,
            }
        }
        (COMMIT_TRAJECTORY_KIND, [slot]) => ControlCommand::CommitTrajectory(*slot),
        (SELECT_TRAJECTORY_KIND, [slot]) => ControlCommand::SelectTrajectory(*slot),
        (SUBSCRIBE_INPUT_KIND, [enabled]) if *enabled <= 1 => {
            ControlCommand::SubscribeInput(*enabled != 0)
        }
        (QUERY_STATUS_KIND, []) => ControlCommand::QueryStatus,
        (SAVE_TRAJECTORIES_KIND, []) => ControlCommand::SaveTrajectories,
        (PROBE_FLASH_KIND, [a, b, c, d]) => {
            ControlCommand::ProbeFlash(u32::from_le_bytes([*a, *b, *c, *d]))
        }
        (QUERY_FLASH_KIND, []) => ControlCommand::QueryFlash,
        (QUERY_STATUS_PAGE_KIND, [page]) => ControlCommand::QueryStatusPage(*page),
        (1..=QUERY_STATUS_PAGE_KIND, _) => return Err(DecodeError::BadPayload),
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
    output[5] = ACK_KIND;
    output[6] = 2;
    output[7] = 0;
    output[8..12].copy_from_slice(&sequence.to_le_bytes());
    output[12] = command_kind;
    output[13] = status;
    Ok(length)
}

/// Encode the latest full physical keyboard/button state. Motion is omitted:
/// this event reports key transitions without flooding the control channel.
pub fn encode_input_event(
    output: &mut [u8],
    sequence: u32,
    modifiers: u8,
    keys: &[u8; 14],
    mouse_buttons: u8,
) -> Result<usize, ()> {
    let payload_len = 16;
    let length = HEADER_LEN + payload_len;
    if output.len() < length {
        return Err(());
    }
    encode_header(output, INPUT_EVENT_KIND, payload_len as u8, sequence);
    output[12] = modifiers;
    output[13..27].copy_from_slice(keys);
    output[27] = mouse_buttons;
    Ok(length)
}

/// Encode the four-slot compatibility status. Per slot: length, tick and CRC.
pub fn encode_status_event(
    output: &mut [u8],
    sequence: u32,
    active_slot: Option<u8>,
    valid_mask: u8,
    running: bool,
    slots: &[(u16, u16, u32); 4],
) -> Result<usize, ()> {
    let payload_len = 3 + slots.len() * 8;
    let length = HEADER_LEN + payload_len;
    if output.len() < length {
        return Err(());
    }
    encode_header(output, STATUS_EVENT_KIND, payload_len as u8, sequence);
    output[12] = active_slot.unwrap_or(u8::MAX);
    output[13] = valid_mask;
    output[14] = u8::from(running);
    let mut cursor = 15;
    for (slot_length, tick_us, crc32) in slots {
        output[cursor..cursor + 2].copy_from_slice(&slot_length.to_le_bytes());
        output[cursor + 2..cursor + 4].copy_from_slice(&tick_us.to_le_bytes());
        output[cursor + 4..cursor + 8].copy_from_slice(&crc32.to_le_bytes());
        cursor += 8;
    }
    Ok(length)
}

/// Encode one page of the extended 16-slot trajectory status. The six-byte
/// page header is followed by up to seven legacy eight-byte slot records, so
/// the event remains within RTCP v1's 64-byte payload limit.
pub fn encode_status_page_event(
    output: &mut [u8],
    sequence: u32,
    page: u8,
    active_slot: Option<u8>,
    valid_mask: u16,
    running: bool,
    slots: &[(u16, u16, u32); MAX_TRAJECTORY_SLOTS],
) -> Result<usize, ()> {
    let start = usize::from(page).saturating_mul(STATUS_PAGE_SLOTS);
    if start >= slots.len() {
        return Err(());
    }
    let count = core::cmp::min(STATUS_PAGE_SLOTS, slots.len() - start);
    let payload_len = STATUS_PAGE_HEADER_LEN + count * 8;
    let length = HEADER_LEN + payload_len;
    if output.len() < length {
        return Err(());
    }
    encode_header(output, STATUS_PAGE_EVENT_KIND, payload_len as u8, sequence);
    output[12] = active_slot.unwrap_or(u8::MAX);
    output[13] = u8::from(running);
    output[14] = MAX_TRAJECTORY_SLOTS as u8;
    output[15] = start as u8;
    output[16..18].copy_from_slice(&valid_mask.to_le_bytes());
    let mut cursor = 18;
    for (slot_length, tick_us, crc32) in &slots[start..start + count] {
        output[cursor..cursor + 2].copy_from_slice(&slot_length.to_le_bytes());
        output[cursor + 2..cursor + 4].copy_from_slice(&tick_us.to_le_bytes());
        output[cursor + 4..cursor + 8].copy_from_slice(&crc32.to_le_bytes());
        cursor += 8;
    }
    Ok(length)
}

fn encode_header(output: &mut [u8], kind: u8, payload_len: u8, sequence: u32) {
    output[..4].copy_from_slice(&MAGIC);
    output[4] = VERSION;
    output[5] = kind;
    output[6] = payload_len;
    output[7] = 0;
    output[8..12].copy_from_slice(&sequence.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(kind: u8, sequence: u32, payload: &[u8]) -> [u8; HEADER_LEN + MAX_PAYLOAD_LEN] {
        let mut packet = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
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
            (
                SET_KEY_KIND,
                &[0x1a, 1][..],
                ControlCommand::SetKey {
                    usage: 0x1a,
                    pressed: true,
                },
            ),
            (
                SET_MOUSE_BUTTONS_KIND,
                &[5][..],
                ControlCommand::SetMouseButtons(5),
            ),
            (
                MOVE_MOUSE_KIND,
                &[0x2c, 0x01, 0x85, 0xff, 0xff, 0x01][..],
                ControlCommand::MoveMouse {
                    x: 300,
                    y: -123,
                    wheel: -1,
                    pan: 1,
                },
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
        let packet = command(TRAJECTORY_CHUNK_KIND, 1, &[0, 0, 0, 1, 1, 2, 3]);
        assert_eq!(decode(&packet[..19]), Err(DecodeError::BadPayload));
    }

    #[test]
    fn decodes_runtime_trajectory_and_subscription_commands() {
        let begin_payload = [2, 0xb0, 0x04, 0xd0, 0x07, 0x78, 0x56, 0x34, 0x12];
        let packet = command(BEGIN_TRAJECTORY_KIND, 7, &begin_payload);
        assert_eq!(
            decode(&packet[..HEADER_LEN + begin_payload.len()]),
            Ok(CommandFrame {
                sequence: 7,
                command: ControlCommand::BeginTrajectory {
                    slot: 2,
                    length: 1_200,
                    tick_us: 2_000,
                    crc32: 0x1234_5678,
                }
            })
        );

        let chunk_payload = [0, 15, 0, 2, 1, 0, 2, 0, 0xff, 0xff, 0xfe, 0xff];
        let packet = command(TRAJECTORY_CHUNK_KIND, 8, &chunk_payload);
        let mut packed = [0u8; MAX_TRAJECTORY_CHUNK_POINTS * 4];
        packed[..8].copy_from_slice(&chunk_payload[4..]);
        assert_eq!(
            decode(&packet[..HEADER_LEN + chunk_payload.len()]),
            Ok(CommandFrame {
                sequence: 8,
                command: ControlCommand::TrajectoryChunk {
                    slot: 0,
                    offset: 15,
                    count: 2,
                    packed,
                }
            })
        );

        for (kind, payload, expected) in [
            (
                COMMIT_TRAJECTORY_KIND,
                &[1][..],
                ControlCommand::CommitTrajectory(1),
            ),
            (
                SELECT_TRAJECTORY_KIND,
                &[u8::MAX][..],
                ControlCommand::SelectTrajectory(u8::MAX),
            ),
            (
                SUBSCRIBE_INPUT_KIND,
                &[1][..],
                ControlCommand::SubscribeInput(true),
            ),
            (QUERY_STATUS_KIND, &[][..], ControlCommand::QueryStatus),
            (
                SAVE_TRAJECTORIES_KIND,
                &[][..],
                ControlCommand::SaveTrajectories,
            ),
            (
                PROBE_FLASH_KIND,
                &0x61fe_0000u32.to_le_bytes()[..],
                ControlCommand::ProbeFlash(0x61fe_0000),
            ),
            (QUERY_FLASH_KIND, &[][..], ControlCommand::QueryFlash),
            (
                QUERY_STATUS_PAGE_KIND,
                &[2][..],
                ControlCommand::QueryStatusPage(2),
            ),
        ] {
            let packet = command(kind, 9, payload);
            assert_eq!(
                decode(&packet[..HEADER_LEN + payload.len()]),
                Ok(CommandFrame {
                    sequence: 9,
                    command: expected
                })
            );
        }
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
    fn encodes_input_and_trajectory_status_events() {
        let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD_LEN];
        let keys = [0x5a; 14];
        let length = encode_input_event(&mut output, 4, 3, &keys, 5).unwrap();
        assert_eq!(length, 28);
        assert_eq!(output[5], INPUT_EVENT_KIND);
        assert_eq!(&output[12..28], &[&[3][..], &keys, &[5][..]].concat());

        let slots = [
            (100, 2_000, 1),
            (200, 5_000, 2),
            (0, 0, 0),
            (1_200, 2_000, 4),
        ];
        let length = encode_status_event(&mut output, 5, Some(1), 0b1011, true, &slots).unwrap();
        assert_eq!(length, 47);
        assert_eq!(output[5], STATUS_EVENT_KIND);
        assert_eq!(&output[12..15], &[1, 0b1011, 1]);
        assert_eq!(&output[15..17], &100u16.to_le_bytes());
        assert_eq!(&output[19..23], &1u32.to_le_bytes());

        let extended_slots =
            core::array::from_fn(|slot| (slot as u16 + 1, 2_500, 0x1000_0000 + slot as u32));
        let length =
            encode_status_page_event(&mut output, 6, 2, Some(15), 0x8001, false, &extended_slots)
                .unwrap();
        assert_eq!(length, HEADER_LEN + STATUS_PAGE_HEADER_LEN + 2 * 8);
        assert_eq!(output[5], STATUS_PAGE_EVENT_KIND);
        assert_eq!(&output[12..18], &[15, 0, 16, 14, 0x01, 0x80]);
        assert_eq!(&output[18..20], &15u16.to_le_bytes());
        assert_eq!(&output[22..26], &0x1000_000eu32.to_le_bytes());
        assert!(
            encode_status_page_event(
                &mut output,
                7,
                STATUS_PAGE_COUNT as u8,
                None,
                0,
                false,
                &extended_slots,
            )
            .is_err()
        );
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

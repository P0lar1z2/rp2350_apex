//! Volatile, allocation-free recoil trajectories uploaded at runtime.

pub const MAX_TRAJECTORY_SLOTS: usize = 4;
pub const MAX_TRAJECTORY_POINTS: usize = 1_536;
pub const MIN_TICK_US: u16 = 1_000;
pub const MAX_TICK_US: u16 = 50_000;
const NO_SLOT: u8 = u8::MAX;
const MAX_CATCH_UP_POINTS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrajectoryPoint {
    pub x: i16,
    pub y: i16,
}

#[derive(Clone, Copy)]
struct TrajectorySlot {
    points: [TrajectoryPoint; MAX_TRAJECTORY_POINTS],
    length: u16,
    tick_us: u16,
    crc32: u32,
    valid: bool,
}

impl TrajectorySlot {
    const fn empty() -> Self {
        Self {
            points: [TrajectoryPoint { x: 0, y: 0 }; MAX_TRAJECTORY_POINTS],
            length: 0,
            tick_us: 0,
            crc32: 0,
            valid: false,
        }
    }
}

#[derive(Clone, Copy)]
struct UploadState {
    points: [TrajectoryPoint; MAX_TRAJECTORY_POINTS],
    target_slot: u8,
    length: u16,
    tick_us: u16,
    expected_crc32: u32,
    received: u16,
    active: bool,
}

impl UploadState {
    const fn empty() -> Self {
        Self {
            points: [TrajectoryPoint { x: 0, y: 0 }; MAX_TRAJECTORY_POINTS],
            target_slot: NO_SLOT,
            length: 0,
            tick_us: 0,
            expected_crc32: 0,
            received: 0,
            active: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrajectoryError {
    BadSlot,
    BadLength,
    BadTick,
    NoUpload,
    WrongSlot,
    OutOfOrder,
    BadChunk,
    Incomplete,
    CrcMismatch,
    EmptySlot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SlotStatus {
    pub valid: bool,
    pub length: u16,
    pub tick_us: u16,
    pub crc32: u32,
}

pub struct RuntimeTrajectoryEngine {
    slots: [TrajectorySlot; MAX_TRAJECTORY_SLOTS],
    upload: UploadState,
    active_slot: u8,
    running: bool,
    trigger_was_down: bool,
    point_index: u16,
    next_due_us: u32,
}

impl RuntimeTrajectoryEngine {
    pub const fn new() -> Self {
        Self {
            slots: [TrajectorySlot::empty(); MAX_TRAJECTORY_SLOTS],
            upload: UploadState::empty(),
            active_slot: NO_SLOT,
            running: false,
            trigger_was_down: false,
            point_index: 0,
            next_due_us: 0,
        }
    }

    /// Start a transactional upload. The currently committed target slot stays
    /// usable until `commit_upload` validates and copies the staging buffer.
    pub fn begin_upload(
        &mut self,
        slot: u8,
        length: u16,
        tick_us: u16,
        expected_crc32: u32,
    ) -> Result<(), TrajectoryError> {
        if usize::from(slot) >= MAX_TRAJECTORY_SLOTS {
            return Err(TrajectoryError::BadSlot);
        }
        if length == 0 || usize::from(length) > MAX_TRAJECTORY_POINTS {
            return Err(TrajectoryError::BadLength);
        }
        if !(MIN_TICK_US..=MAX_TICK_US).contains(&tick_us) {
            return Err(TrajectoryError::BadTick);
        }
        self.upload.target_slot = slot;
        self.upload.length = length;
        self.upload.tick_us = tick_us;
        self.upload.expected_crc32 = expected_crc32;
        self.upload.received = 0;
        self.upload.active = true;
        Ok(())
    }

    /// Append packed little-endian `(x: i16, y: i16)` points. Sequential
    /// offsets make missing or reordered UDP chunks unambiguous.
    pub fn write_chunk(
        &mut self,
        slot: u8,
        offset: u16,
        count: u8,
        packed_points: &[u8],
    ) -> Result<(), TrajectoryError> {
        if !self.upload.active {
            return Err(TrajectoryError::NoUpload);
        }
        if slot != self.upload.target_slot {
            return Err(TrajectoryError::WrongSlot);
        }
        if offset != self.upload.received {
            return Err(TrajectoryError::OutOfOrder);
        }
        let count = usize::from(count);
        if count == 0 || packed_points.len() != count.saturating_mul(4) {
            return Err(TrajectoryError::BadChunk);
        }
        let end = usize::from(offset).saturating_add(count);
        if end > usize::from(self.upload.length) || end > MAX_TRAJECTORY_POINTS {
            return Err(TrajectoryError::BadChunk);
        }
        for (index, bytes) in packed_points.chunks_exact(4).enumerate() {
            self.upload.points[usize::from(offset) + index] = TrajectoryPoint {
                x: i16::from_le_bytes([bytes[0], bytes[1]]),
                y: i16::from_le_bytes([bytes[2], bytes[3]]),
            };
        }
        self.upload.received = end as u16;
        Ok(())
    }

    pub fn commit_upload(&mut self, slot: u8) -> Result<(), TrajectoryError> {
        if !self.upload.active {
            return Err(TrajectoryError::NoUpload);
        }
        if slot != self.upload.target_slot {
            return Err(TrajectoryError::WrongSlot);
        }
        if self.upload.received != self.upload.length {
            return Err(TrajectoryError::Incomplete);
        }
        let actual_crc32 = crc32_points(&self.upload.points[..usize::from(self.upload.length)]);
        if actual_crc32 != self.upload.expected_crc32 {
            self.upload.active = false;
            return Err(TrajectoryError::CrcMismatch);
        }

        let target = &mut self.slots[usize::from(slot)];
        let length = usize::from(self.upload.length);
        target.points[..length].copy_from_slice(&self.upload.points[..length]);
        target.length = self.upload.length;
        target.tick_us = self.upload.tick_us;
        target.crc32 = actual_crc32;
        target.valid = true;
        self.upload.active = false;
        if self.active_slot == slot {
            self.cancel_playback();
        }
        Ok(())
    }

    pub fn select_slot(&mut self, slot: u8) -> Result<(), TrajectoryError> {
        let Some(selected) = self.slots.get(usize::from(slot)) else {
            return Err(TrajectoryError::BadSlot);
        };
        if !selected.valid {
            return Err(TrajectoryError::EmptySlot);
        }
        self.active_slot = slot;
        self.cancel_playback();
        Ok(())
    }

    pub const fn active_slot(&self) -> Option<u8> {
        if self.active_slot == NO_SLOT {
            None
        } else {
            Some(self.active_slot)
        }
    }

    pub const fn is_running(&self) -> bool {
        self.running
    }

    pub fn valid_mask(&self) -> u8 {
        self.slots
            .iter()
            .enumerate()
            .fold(0u8, |mask, (index, slot)| {
                if slot.valid {
                    mask | (1 << index)
                } else {
                    mask
                }
            })
    }

    pub fn slot_status(&self, slot: u8) -> Option<SlotStatus> {
        self.slots.get(usize::from(slot)).map(|value| SlotStatus {
            valid: value.valid,
            length: value.length,
            tick_us: value.tick_us,
            crc32: value.crc32,
        })
    }

    /// Advance the selected trajectory. Multiple overdue points are combined
    /// into one HID delta, with bounded catch-up work per main-loop pass.
    pub fn tick(&mut self, now_us: u32, trigger_down: bool) -> Option<TrajectoryPoint> {
        if !trigger_down {
            self.cancel_playback();
            self.trigger_was_down = false;
            return None;
        }
        if !self.trigger_was_down {
            self.trigger_was_down = true;
            if let Some(slot) = self.active_slot()
                && self.slots[usize::from(slot)].valid
            {
                self.running = true;
                self.point_index = 0;
                self.next_due_us = now_us;
            }
        }
        if !self.running {
            return None;
        }

        let slot = &self.slots[usize::from(self.active_slot)];
        let mut output = TrajectoryPoint::default();
        let mut emitted = false;
        for _ in 0..MAX_CATCH_UP_POINTS {
            if (now_us.wrapping_sub(self.next_due_us) as i32) < 0 {
                break;
            }
            if self.point_index >= slot.length {
                self.running = false;
                break;
            }
            let point = slot.points[usize::from(self.point_index)];
            output.x = output.x.saturating_add(point.x);
            output.y = output.y.saturating_add(point.y);
            emitted = true;
            self.point_index += 1;
            self.next_due_us = self.next_due_us.wrapping_add(u32::from(slot.tick_us));
        }
        if self.point_index >= slot.length {
            self.running = false;
        }
        emitted.then_some(output)
    }

    pub fn cancel_playback(&mut self) {
        self.running = false;
        self.point_index = 0;
        self.next_due_us = 0;
    }
}

impl Default for RuntimeTrajectoryEngine {
    fn default() -> Self {
        Self::new()
    }
}

pub fn crc32_points(points: &[TrajectoryPoint]) -> u32 {
    let mut crc = u32::MAX;
    for point in points {
        for byte in point
            .x
            .to_le_bytes()
            .into_iter()
            .chain(point.y.to_le_bytes())
        {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packed(points: &[TrajectoryPoint], output: &mut [u8]) -> usize {
        let mut length = 0;
        for point in points {
            output[length..length + 2].copy_from_slice(&point.x.to_le_bytes());
            output[length + 2..length + 4].copy_from_slice(&point.y.to_le_bytes());
            length += 4;
        }
        length
    }

    #[test]
    fn upload_commit_select_and_play() {
        let points = [
            TrajectoryPoint { x: 1, y: 2 },
            TrajectoryPoint { x: -2, y: 3 },
            TrajectoryPoint { x: 4, y: -5 },
        ];
        let mut bytes = [0u8; 12];
        let length = packed(&points, &mut bytes);
        let mut engine = RuntimeTrajectoryEngine::new();
        engine
            .begin_upload(2, points.len() as u16, 2_000, crc32_points(&points))
            .unwrap();
        engine.write_chunk(2, 0, 2, &bytes[..8]).unwrap();
        engine.write_chunk(2, 2, 1, &bytes[8..length]).unwrap();
        engine.commit_upload(2).unwrap();
        engine.select_slot(2).unwrap();

        assert_eq!(engine.valid_mask(), 1 << 2);
        assert_eq!(engine.tick(10_000, true), Some(points[0]));
        assert_eq!(engine.tick(11_000, true), None);
        assert_eq!(
            engine.tick(14_500, true),
            Some(TrajectoryPoint { x: 2, y: -2 })
        );
        assert!(!engine.is_running());
        assert_eq!(engine.tick(20_000, true), None);
        assert_eq!(engine.tick(21_000, false), None);
        assert_eq!(engine.tick(22_000, true), Some(points[0]));
    }

    #[test]
    fn upload_is_ordered_crc_checked_and_transactional() {
        let old = [TrajectoryPoint { x: 7, y: 8 }];
        let mut old_bytes = [0u8; 4];
        packed(&old, &mut old_bytes);
        let mut engine = RuntimeTrajectoryEngine::new();
        engine
            .begin_upload(0, 1, 5_000, crc32_points(&old))
            .unwrap();
        engine.write_chunk(0, 0, 1, &old_bytes).unwrap();
        engine.commit_upload(0).unwrap();
        engine.select_slot(0).unwrap();

        engine.begin_upload(0, 1, 2_000, 0x1234_5678).unwrap();
        assert_eq!(
            engine.write_chunk(0, 1, 1, &old_bytes),
            Err(TrajectoryError::OutOfOrder)
        );
        engine.write_chunk(0, 0, 1, &old_bytes).unwrap();
        assert_eq!(engine.commit_upload(0), Err(TrajectoryError::CrcMismatch));

        assert_eq!(engine.tick(100, true), Some(old[0]));
        assert_eq!(engine.slot_status(0).unwrap().tick_us, 5_000);
    }

    #[test]
    fn validates_slot_length_tick_and_empty_selection() {
        let mut engine = RuntimeTrajectoryEngine::new();
        assert_eq!(
            engine.begin_upload(4, 1, 2_000, 0),
            Err(TrajectoryError::BadSlot)
        );
        assert_eq!(
            engine.begin_upload(0, 0, 2_000, 0),
            Err(TrajectoryError::BadLength)
        );
        assert_eq!(
            engine.begin_upload(0, 1, 999, 0),
            Err(TrajectoryError::BadTick)
        );
        assert_eq!(engine.select_slot(0), Err(TrajectoryError::EmptySlot));
    }
}

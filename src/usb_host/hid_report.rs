//! Allocation-free parser for common HID input Report Descriptors.

pub const KEY_BITMAP_BYTES: usize = 14; // Keyboard usages 0x04..=0x73.
const MAX_LAYOUTS: usize = 8;
const MAX_LOCAL_USAGES: usize = 16;
const MAX_COLLECTION_DEPTH: usize = 8;
const MAX_GLOBAL_DEPTH: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardState {
    pub modifiers: u8,
    pub keys: [u8; KEY_BITMAP_BYTES],
}

impl KeyboardState {
    pub const fn empty() -> Self {
        Self {
            modifiers: 0,
            keys: [0; KEY_BITMAP_BYTES],
        }
    }

    pub fn press(&mut self, usage: u16) {
        if (0x04..=0x73).contains(&usage) {
            let bit = usize::from(usage - 0x04);
            self.keys[bit / 8] |= 1 << (bit % 8);
        }
    }

    pub fn release(&mut self, usage: u16) {
        if (0x04..=0x73).contains(&usage) {
            let bit = usize::from(usage - 0x04);
            self.keys[bit / 8] &= !(1 << (bit % 8));
        }
    }

    pub fn is_pressed(&self, usage: u16) -> bool {
        if !(0x04..=0x73).contains(&usage) {
            return false;
        }
        let bit = usize::from(usage - 0x04);
        self.keys[bit / 8] & (1 << (bit % 8)) != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseState {
    pub buttons: u8,
    pub x: i16,
    pub y: i16,
    pub wheel: i8,
    pub pan: i8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodedReport {
    Keyboard(KeyboardState),
    Mouse(MouseState),
    Consumer(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportEncodeError {
    NoMatchingLayout,
    BufferTooSmall,
    TooManyKeys,
    UnsupportedField,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LayoutKind {
    Unknown,
    Keyboard,
    Mouse,
    Consumer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BitField {
    offset: u16,
    size: u8,
    count: u8,
    usage_min: u16,
    logical_min: i32,
    logical_max: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseLayout {
    report_id: u8,
    buttons: Option<BitField>,
    x: Option<BitField>,
    y: Option<BitField>,
    wheel: Option<BitField>,
    pan: Option<BitField>,
}

impl MouseLayout {
    const fn new(report_id: u8) -> Self {
        Self {
            report_id,
            buttons: None,
            x: None,
            y: None,
            wheel: None,
            pan: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KeyboardLayout {
    report_id: u8,
    modifiers: Option<BitField>,
    key_array: Option<BitField>,
    key_bitmap: Option<BitField>,
}

impl KeyboardLayout {
    const fn new(report_id: u8) -> Self {
        Self {
            report_id,
            modifiers: None,
            key_array: None,
            key_bitmap: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ConsumerLayout {
    report_id: u8,
    array: Option<BitField>,
    bitmap: Option<BitField>,
}

impl ConsumerLayout {
    const fn new(report_id: u8) -> Self {
        Self {
            report_id,
            array: None,
            bitmap: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportLayout {
    Keyboard(KeyboardLayout),
    Mouse(MouseLayout),
    Consumer(ConsumerLayout),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReportSize {
    report_id: u8,
    bits: u16,
}

impl ReportLayout {
    const fn report_id(self) -> u8 {
        match self {
            Self::Keyboard(layout) => layout.report_id,
            Self::Mouse(layout) => layout.report_id,
            Self::Consumer(layout) => layout.report_id,
        }
    }

    const fn kind(self) -> LayoutKind {
        match self {
            Self::Keyboard(_) => LayoutKind::Keyboard,
            Self::Mouse(_) => LayoutKind::Mouse,
            Self::Consumer(_) => LayoutKind::Consumer,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportDecoder {
    layouts: [Option<ReportLayout>; MAX_LAYOUTS],
    report_sizes: [Option<ReportSize>; MAX_LAYOUTS],
    uses_report_ids: bool,
}

impl ReportDecoder {
    pub const fn empty() -> Self {
        Self {
            layouts: [None; MAX_LAYOUTS],
            report_sizes: [None; MAX_LAYOUTS],
            uses_report_ids: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.layouts.iter().all(Option::is_none)
    }

    pub fn decode(&self, report: &[u8]) -> Option<DecodedReport> {
        let (report_id, payload) = if self.uses_report_ids {
            (report.first().copied()?, report.get(1..)?)
        } else {
            (0, report)
        };
        let layout = self
            .layouts
            .iter()
            .flatten()
            .copied()
            .find(|layout| layout.report_id() == report_id)?;
        match layout {
            ReportLayout::Mouse(layout) => decode_mouse(layout, payload),
            ReportLayout::Keyboard(layout) => decode_keyboard(layout, payload),
            ReportLayout::Consumer(layout) => decode_consumer(layout, payload),
        }
    }

    /// Overwrite the known fields of an existing input report while retaining
    /// padding and vendor-defined fields. When Report IDs are in use, the ID
    /// already present in `report` selects the layout and is left unchanged.
    pub fn encode(
        &self,
        decoded: &DecodedReport,
        report: &mut [u8],
    ) -> Result<(), ReportEncodeError> {
        let (report_id, payload) = if self.uses_report_ids {
            let (report_id, payload) = report
                .split_first_mut()
                .ok_or(ReportEncodeError::BufferTooSmall)?;
            (*report_id, payload)
        } else {
            (0, report)
        };
        let wanted_kind = match decoded {
            DecodedReport::Keyboard(_) => LayoutKind::Keyboard,
            DecodedReport::Mouse(_) => LayoutKind::Mouse,
            DecodedReport::Consumer(_) => LayoutKind::Consumer,
        };
        let layout = self
            .layouts
            .iter()
            .flatten()
            .copied()
            .find(|layout| layout.kind() == wanted_kind && layout.report_id() == report_id)
            .ok_or(ReportEncodeError::NoMatchingLayout)?;
        match (layout, decoded) {
            (ReportLayout::Keyboard(layout), DecodedReport::Keyboard(state)) => {
                encode_keyboard(layout, *state, payload)
            }
            (ReportLayout::Mouse(layout), DecodedReport::Mouse(state)) => {
                encode_mouse(layout, *state, payload)
            }
            (ReportLayout::Consumer(layout), DecodedReport::Consumer(usage)) => {
                encode_consumer(layout, *usage, payload)
            }
            _ => Err(ReportEncodeError::NoMatchingLayout),
        }
    }

    /// Create a zero-initialized report for the first layout matching
    /// `decoded`, including its Report ID when required, then encode it.
    pub fn encode_new(
        &self,
        decoded: &DecodedReport,
        report: &mut [u8],
    ) -> Result<usize, ReportEncodeError> {
        let wanted_kind = match decoded {
            DecodedReport::Keyboard(_) => LayoutKind::Keyboard,
            DecodedReport::Mouse(_) => LayoutKind::Mouse,
            DecodedReport::Consumer(_) => LayoutKind::Consumer,
        };
        let layout = self
            .layouts
            .iter()
            .flatten()
            .copied()
            .find(|layout| layout.kind() == wanted_kind)
            .ok_or(ReportEncodeError::NoMatchingLayout)?;
        let size = self
            .report_sizes
            .iter()
            .flatten()
            .find(|size| size.report_id == layout.report_id())
            .ok_or(ReportEncodeError::NoMatchingLayout)?;
        let payload_len = usize::from(size.bits).div_ceil(8);
        let report_len = payload_len + usize::from(self.uses_report_ids);
        if report.len() < report_len {
            return Err(ReportEncodeError::BufferTooSmall);
        }
        report[..report_len].fill(0);
        if self.uses_report_ids {
            report[0] = layout.report_id();
        }
        self.encode(decoded, &mut report[..report_len])?;
        Ok(report_len)
    }

    fn layout_mut(&mut self, kind: LayoutKind, report_id: u8) -> Option<&mut ReportLayout> {
        if let Some(index) = self.layouts.iter().position(|slot| {
            slot.is_some_and(|layout| layout.kind() == kind && layout.report_id() == report_id)
        }) {
            return self.layouts[index].as_mut();
        }
        let index = self.layouts.iter().position(Option::is_none)?;
        self.layouts[index] = Some(match kind {
            LayoutKind::Keyboard => ReportLayout::Keyboard(KeyboardLayout::new(report_id)),
            LayoutKind::Mouse => ReportLayout::Mouse(MouseLayout::new(report_id)),
            LayoutKind::Consumer => ReportLayout::Consumer(ConsumerLayout::new(report_id)),
            LayoutKind::Unknown => return None,
        });
        self.layouts[index].as_mut()
    }
}

#[derive(Clone, Copy)]
struct Globals {
    usage_page: u16,
    logical_min: i32,
    logical_max: i32,
    report_size: u8,
    report_count: u8,
    report_id: u8,
}

impl Globals {
    const fn new() -> Self {
        Self {
            usage_page: 0,
            logical_min: 0,
            logical_max: 0,
            report_size: 0,
            report_count: 0,
            report_id: 0,
        }
    }
}

struct Locals {
    usages: [u32; MAX_LOCAL_USAGES],
    usage_count: usize,
    usage_min: Option<u32>,
    usage_max: Option<u32>,
}

impl Locals {
    const fn new() -> Self {
        Self {
            usages: [0; MAX_LOCAL_USAGES],
            usage_count: 0,
            usage_min: None,
            usage_max: None,
        }
    }

    fn clear(&mut self) {
        self.usage_count = 0;
        self.usage_min = None;
        self.usage_max = None;
    }

    fn push_usage(&mut self, usage: u32) {
        if self.usage_count < self.usages.len() {
            self.usages[self.usage_count] = usage;
            self.usage_count += 1;
        }
    }

    fn usage(&self, index: usize, page: u16) -> Option<(u16, u16)> {
        let raw = if index < self.usage_count {
            self.usages[index]
        } else if let Some(minimum) = self.usage_min {
            let maximum = self.usage_max.unwrap_or(minimum);
            minimum.saturating_add(index as u32).min(maximum)
        } else if self.usage_count != 0 {
            self.usages[self.usage_count - 1]
        } else {
            return None;
        };
        let usage_page = if raw > 0xffff {
            (raw >> 16) as u16
        } else {
            page
        };
        Some((usage_page, raw as u16))
    }
}

#[derive(Clone, Copy)]
struct Offset {
    report_id: u8,
    bits: u16,
}

/// Parse the common keyboard, mouse and Consumer Control portions of a HID
/// Report Descriptor. Unknown and unsupported items are skipped.
pub fn parse_report_descriptor(bytes: &[u8]) -> ReportDecoder {
    let mut decoder = ReportDecoder::empty();
    let mut globals = Globals::new();
    let mut global_stack = [Globals::new(); MAX_GLOBAL_DEPTH];
    let mut global_depth = 0usize;
    let mut locals = Locals::new();
    let mut collections = [LayoutKind::Unknown; MAX_COLLECTION_DEPTH];
    let mut collection_depth = 0usize;
    let mut offsets = [Offset {
        report_id: 0,
        bits: 0,
    }; MAX_LAYOUTS];
    let mut offset_count = 1usize;
    let mut index = 0usize;

    while index < bytes.len() {
        let prefix = bytes[index];
        index += 1;
        if prefix == 0xfe {
            if index + 2 > bytes.len() {
                break;
            }
            let size = usize::from(bytes[index]);
            index = index.saturating_add(2 + size).min(bytes.len());
            continue;
        }
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        if index + size > bytes.len() {
            break;
        }
        let data = read_unsigned(&bytes[index..index + size]);
        let signed = read_signed(data, size);
        index += size;
        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;

        match (item_type, tag) {
            // Global items.
            (1, 0) => globals.usage_page = data as u16,
            (1, 1) => globals.logical_min = signed,
            (1, 2) => {
                globals.logical_max = if globals.logical_min < 0 {
                    signed
                } else {
                    data as i32
                }
            }
            (1, 7) => globals.report_size = data.min(32) as u8,
            (1, 8) => {
                globals.report_id = data as u8;
                decoder.uses_report_ids = true;
                if !offsets[..offset_count]
                    .iter()
                    .any(|offset| offset.report_id == globals.report_id)
                    && offset_count < offsets.len()
                {
                    offsets[offset_count] = Offset {
                        report_id: globals.report_id,
                        bits: 0,
                    };
                    offset_count += 1;
                }
            }
            (1, 9) => globals.report_count = data.min(255) as u8,
            (1, 10) if global_depth < global_stack.len() => {
                global_stack[global_depth] = globals;
                global_depth += 1;
            }
            (1, 11) if global_depth != 0 => {
                global_depth -= 1;
                globals = global_stack[global_depth];
            }
            // Local items.
            (2, 0) => locals.push_usage(data),
            (2, 1) => locals.usage_min = Some(data),
            (2, 2) => locals.usage_max = Some(data),
            // Collection.
            (0, 10) => {
                let inherited = collection_depth
                    .checked_sub(1)
                    .map_or(LayoutKind::Unknown, |depth| collections[depth]);
                let kind = if data as u8 == 1 {
                    locals
                        .usage(0, globals.usage_page)
                        .map_or(inherited, |usage| {
                            kind_for_application(usage).unwrap_or(inherited)
                        })
                } else {
                    inherited
                };
                if collection_depth < collections.len() {
                    collections[collection_depth] = kind;
                    collection_depth += 1;
                }
                locals.clear();
            }
            // End Collection.
            (0, 12) => {
                collection_depth = collection_depth.saturating_sub(1);
                locals.clear();
            }
            // Input.
            (0, 8) => {
                let kind = collection_depth
                    .checked_sub(1)
                    .map_or(LayoutKind::Unknown, |depth| collections[depth]);
                let offset = offsets[..offset_count]
                    .iter_mut()
                    .find(|offset| offset.report_id == globals.report_id);
                if let Some(offset) = offset {
                    let field = BitField {
                        offset: offset.bits,
                        size: globals.report_size,
                        count: globals.report_count,
                        usage_min: locals
                            .usage(0, globals.usage_page)
                            .map_or(0, |(_, usage)| usage),
                        logical_min: globals.logical_min,
                        logical_max: globals.logical_max,
                    };
                    let constant = data & 1 != 0;
                    let variable = data & 2 != 0;
                    if !constant && field.size != 0 && field.count != 0 {
                        apply_input_field(
                            &mut decoder,
                            kind,
                            globals.report_id,
                            field,
                            variable,
                            &locals,
                            globals.usage_page,
                        );
                    }
                    offset.bits = offset.bits.saturating_add(
                        u16::from(globals.report_size) * u16::from(globals.report_count),
                    );
                }
                locals.clear();
            }
            // Other Main items still clear local state.
            (0, _) => locals.clear(),
            _ => {}
        }
    }
    for (index, offset) in offsets[..offset_count].iter().copied().enumerate() {
        decoder.report_sizes[index] = Some(ReportSize {
            report_id: offset.report_id,
            bits: offset.bits,
        });
    }
    decoder
}

fn kind_for_application((page, usage): (u16, u16)) -> Option<LayoutKind> {
    match (page, usage) {
        (0x01, 0x02) => Some(LayoutKind::Mouse),
        (0x01, 0x06) => Some(LayoutKind::Keyboard),
        (0x0c, 0x01) => Some(LayoutKind::Consumer),
        _ => None,
    }
}

fn apply_input_field(
    decoder: &mut ReportDecoder,
    kind: LayoutKind,
    report_id: u8,
    field: BitField,
    variable: bool,
    locals: &Locals,
    default_page: u16,
) {
    let Some(layout) = decoder.layout_mut(kind, report_id) else {
        return;
    };
    match layout {
        ReportLayout::Mouse(layout) => {
            if locals
                .usage(0, default_page)
                .is_some_and(|(page, _)| page == 0x09)
            {
                layout.buttons = Some(field);
            }
            if variable {
                for item in 0..usize::from(field.count) {
                    let Some((page, usage)) = locals.usage(item, default_page) else {
                        continue;
                    };
                    let scalar = BitField {
                        offset: field.offset + item as u16 * u16::from(field.size),
                        count: 1,
                        usage_min: usage,
                        ..field
                    };
                    match (page, usage) {
                        (0x01, 0x30) => layout.x = Some(scalar),
                        (0x01, 0x31) => layout.y = Some(scalar),
                        (0x01, 0x38) => layout.wheel = Some(scalar),
                        (0x0c, 0x0238) => layout.pan = Some(scalar),
                        _ => {}
                    }
                }
            }
        }
        ReportLayout::Keyboard(layout) => {
            if default_page != 0x07 {
                return;
            }
            if variable && field.usage_min >= 0xe0 && field.usage_min <= 0xe7 {
                layout.modifiers = Some(field);
            } else if variable {
                layout.key_bitmap = Some(field);
            } else {
                layout.key_array = Some(field);
            }
        }
        ReportLayout::Consumer(layout) => {
            if default_page == 0x0c {
                if variable {
                    layout.bitmap = Some(field);
                } else {
                    layout.array = Some(field);
                }
            }
        }
    }
}

fn decode_mouse(layout: MouseLayout, payload: &[u8]) -> Option<DecodedReport> {
    let mut buttons = 0u8;
    if let Some(field) = layout.buttons {
        for index in 0..field.count.min(8) {
            if extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? != 0
            {
                buttons |= 1 << index;
            }
        }
    }
    Some(DecodedReport::Mouse(MouseState {
        buttons,
        x: signed_field(payload, layout.x).unwrap_or(0) as i16,
        y: signed_field(payload, layout.y).unwrap_or(0) as i16,
        wheel: clamp_i8(signed_field(payload, layout.wheel).unwrap_or(0)),
        pan: clamp_i8(signed_field(payload, layout.pan).unwrap_or(0)),
    }))
}

fn decode_keyboard(layout: KeyboardLayout, payload: &[u8]) -> Option<DecodedReport> {
    let mut state = KeyboardState::empty();
    if let Some(field) = layout.modifiers {
        for index in 0..field.count.min(8) {
            if extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? != 0
            {
                let usage = field.usage_min + u16::from(index);
                if (0xe0..=0xe7).contains(&usage) {
                    state.modifiers |= 1 << (usage - 0xe0);
                }
            }
        }
    }
    if let Some(field) = layout.key_array {
        for index in 0..field.count {
            let usage = extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? as u16;
            state.press(usage);
        }
    }
    if let Some(field) = layout.key_bitmap {
        for index in 0..field.count {
            if extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? != 0
            {
                state.press(field.usage_min + u16::from(index));
            }
        }
    }
    Some(DecodedReport::Keyboard(state))
}

fn decode_consumer(layout: ConsumerLayout, payload: &[u8]) -> Option<DecodedReport> {
    if let Some(field) = layout.array {
        for index in 0..field.count {
            let usage = extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? as u16;
            if usage != 0 {
                return Some(DecodedReport::Consumer(usage));
            }
        }
        return Some(DecodedReport::Consumer(0));
    }
    if let Some(field) = layout.bitmap {
        for index in 0..field.count {
            if extract(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
            )? != 0
            {
                return Some(DecodedReport::Consumer(field.usage_min + u16::from(index)));
            }
        }
        return Some(DecodedReport::Consumer(0));
    }
    None
}

fn encode_mouse(
    layout: MouseLayout,
    state: MouseState,
    payload: &mut [u8],
) -> Result<(), ReportEncodeError> {
    if let Some(field) = layout.buttons {
        for index in 0..field.count.min(8) {
            let pressed = state.buttons & (1 << index) != 0;
            insert(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
                u32::from(pressed),
            )?;
        }
    }
    encode_signed_field(payload, layout.x, i32::from(state.x))?;
    encode_signed_field(payload, layout.y, i32::from(state.y))?;
    encode_signed_field(payload, layout.wheel, i32::from(state.wheel))?;
    encode_signed_field(payload, layout.pan, i32::from(state.pan))?;
    Ok(())
}

fn encode_keyboard(
    layout: KeyboardLayout,
    state: KeyboardState,
    payload: &mut [u8],
) -> Result<(), ReportEncodeError> {
    if let Some(field) = layout.modifiers {
        for index in 0..field.count.min(8) {
            let usage = field.usage_min + u16::from(index);
            let pressed =
                (0xe0..=0xe7).contains(&usage) && state.modifiers & (1 << (usage - 0xe0)) != 0;
            insert(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
                u32::from(pressed),
            )?;
        }
    }
    if let Some(field) = layout.key_bitmap {
        for index in 0..field.count {
            let usage = field.usage_min + u16::from(index);
            if !(0x04..=0x73).contains(&usage) {
                continue;
            }
            insert(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
                u32::from(state.is_pressed(usage)),
            )?;
        }
    }
    if let Some(field) = layout.key_array {
        let mut slot = 0u8;
        for usage in 0x04..=0x73 {
            if !state.is_pressed(usage) {
                continue;
            }
            if slot == field.count {
                return Err(ReportEncodeError::TooManyKeys);
            }
            insert(
                payload,
                field.offset + u16::from(slot) * u16::from(field.size),
                field.size,
                u32::from(usage),
            )?;
            slot += 1;
        }
        while slot < field.count {
            insert(
                payload,
                field.offset + u16::from(slot) * u16::from(field.size),
                field.size,
                0,
            )?;
            slot += 1;
        }
    }
    Ok(())
}

fn encode_consumer(
    layout: ConsumerLayout,
    usage: u16,
    payload: &mut [u8],
) -> Result<(), ReportEncodeError> {
    if let Some(field) = layout.array {
        insert(payload, field.offset, field.size, u32::from(usage))?;
        for index in 1..field.count {
            insert(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
                0,
            )?;
        }
        return Ok(());
    }
    if let Some(field) = layout.bitmap {
        for index in 0..field.count {
            insert(
                payload,
                field.offset + u16::from(index) * u16::from(field.size),
                field.size,
                u32::from(usage != 0 && usage == field.usage_min + u16::from(index)),
            )?;
        }
        return Ok(());
    }
    Err(ReportEncodeError::UnsupportedField)
}

fn encode_signed_field(
    payload: &mut [u8],
    field: Option<BitField>,
    value: i32,
) -> Result<(), ReportEncodeError> {
    let Some(field) = field else {
        return Ok(());
    };
    let value = value.clamp(field.logical_min, field.logical_max.max(field.logical_min));
    insert(payload, field.offset, field.size, value as u32)
}

fn signed_field(payload: &[u8], field: Option<BitField>) -> Option<i32> {
    let field = field?;
    let value = extract(payload, field.offset, field.size)?;
    let value = if field.logical_min < 0 {
        sign_extend(value, field.size)
    } else {
        value.min(i32::MAX as u32) as i32
    };
    Some(value.clamp(field.logical_min, field.logical_max.max(field.logical_min)))
}

fn extract(bytes: &[u8], offset: u16, size: u8) -> Option<u32> {
    if size == 0 || size > 32 || usize::from(offset) + usize::from(size) > bytes.len() * 8 {
        return None;
    }
    let mut value = 0u32;
    for bit in 0..size {
        let source = usize::from(offset) + usize::from(bit);
        value |= u32::from((bytes[source / 8] >> (source % 8)) & 1) << bit;
    }
    Some(value)
}

fn insert(bytes: &mut [u8], offset: u16, size: u8, value: u32) -> Result<(), ReportEncodeError> {
    if size == 0 || size > 32 || usize::from(offset) + usize::from(size) > bytes.len() * 8 {
        return Err(ReportEncodeError::BufferTooSmall);
    }
    for bit in 0..size {
        let destination = usize::from(offset) + usize::from(bit);
        let mask = 1 << (destination % 8);
        if value & (1 << bit) != 0 {
            bytes[destination / 8] |= mask;
        } else {
            bytes[destination / 8] &= !mask;
        }
    }
    Ok(())
}

fn sign_extend(value: u32, size: u8) -> i32 {
    if size == 32 {
        value as i32
    } else {
        let shift = 32 - size;
        ((value << shift) as i32) >> shift
    }
}

fn clamp_i8(value: i32) -> i8 {
    value.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

fn read_unsigned(bytes: &[u8]) -> u32 {
    bytes.iter().enumerate().fold(0, |value, (shift, byte)| {
        value | (u32::from(*byte) << (shift * 8))
    })
}

fn read_signed(value: u32, size: usize) -> i32 {
    if size == 0 {
        0
    } else {
        sign_extend(value, (size * 8) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_report_id_mouse_with_wheel_and_pan() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, // Mouse application
            0x85, 0x02, // Report ID 2
            0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08,
            0x81, 0x02, // Eight buttons
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08,
            0x95, 0x03, 0x81, 0x06, // X/Y/wheel
            0x05, 0x0c, 0x0a, 0x38, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        assert_eq!(
            decoder.decode(&[2, 0x21, 5, 0xfb, 1, 0xff]),
            Some(DecodedReport::Mouse(MouseState {
                buttons: 0x21,
                x: 5,
                y: -5,
                wheel: 1,
                pan: -1,
            }))
        );

        let mut report = [2, 0xa5, 0, 0, 0, 0];
        decoder
            .encode(
                &DecodedReport::Mouse(MouseState {
                    buttons: 0x42,
                    x: -7,
                    y: 12,
                    wheel: -2,
                    pan: 3,
                }),
                &mut report,
            )
            .unwrap();
        assert_eq!(report, [2, 0x42, 0xf9, 12, 0xfe, 3]);
        assert_eq!(
            decoder.decode(&report),
            Some(DecodedReport::Mouse(MouseState {
                buttons: 0x42,
                x: -7,
                y: 12,
                wheel: -2,
                pan: 3,
            }))
        );

        let mut fresh = [0xa5; 16];
        let length = decoder
            .encode_new(
                &DecodedReport::Mouse(MouseState {
                    buttons: 1,
                    x: 2,
                    y: 3,
                    wheel: 4,
                    pan: 5,
                }),
                &mut fresh,
            )
            .unwrap();
        assert_eq!(length, 6);
        assert_eq!(&fresh[..length], &[2, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn decodes_rt1052_captured_mouse_report() {
        // 17ef:62c2, interface 1, fetched through the RT1052 NXP Host stack.
        let descriptor = [
            0x06, 0xb5, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0xb5, 0x09, 0x02, 0x15, 0x00, 0x26,
            0xff, 0x00, 0x75, 0x08, 0x95, 0x07, 0x81, 0x02, 0x09, 0x02, 0x15, 0x00, 0x26, 0xff,
            0x00, 0x75, 0x08, 0x95, 0x07, 0x91, 0x02, 0xc0, 0x05, 0x01, 0x09, 0x02, 0xa1, 0x01,
            0x85, 0x02, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x08, 0x15, 0x00,
            0x25, 0x01, 0x95, 0x08, 0x75, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
            0x16, 0x01, 0xf8, 0x26, 0xff, 0x07, 0x75, 0x0c, 0x95, 0x02, 0x81, 0x06, 0x09, 0x38,
            0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0x05, 0x0c, 0x0a, 0x38,
            0x02, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0, 0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85,
            0x01, 0x19, 0x01, 0x2a, 0xff, 0x03, 0x15, 0x01, 0x26, 0xff, 0x03, 0x95, 0x01, 0x75,
            0x10, 0x81, 0x00, 0xc0, 0x05, 0x01, 0x09, 0x80, 0xa1, 0x01, 0x85, 0x03, 0x1a, 0x81,
            0x00, 0x2a, 0x83, 0x00, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02,
            0x95, 0x05, 0x81, 0x01, 0xc0, 0x06, 0xbc, 0xff, 0x09, 0x88, 0xa1, 0x01, 0x85, 0x04,
            0x19, 0x00, 0x2a, 0xff, 0x00, 0x15, 0x00, 0x26, 0xff, 0x00, 0x95, 0x01, 0x75, 0x08,
            0x81, 0x00, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        assert_eq!(
            decoder.decode(&[0x02, 0x00, 0x02, 0xf0, 0xff, 0x00, 0x00]),
            Some(DecodedReport::Mouse(MouseState {
                buttons: 0,
                x: 2,
                y: -1,
                wheel: 0,
                pan: 0,
            }))
        );
    }

    #[test]
    fn decodes_boot_keyboard_array() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00,
            0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x75, 0x08, 0x95, 0x01, 0x81, 0x01,
            0x19, 0x00, 0x29, 0x65, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        let mut keys = [0; KEY_BITMAP_BYTES];
        keys[0] = 1; // Usage 0x04 (A)
        assert_eq!(
            decoder.decode(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]),
            Some(DecodedReport::Keyboard(KeyboardState {
                modifiers: 0x02,
                keys,
            }))
        );

        let mut state = KeyboardState::empty();
        state.modifiers = 0x11;
        state.press(0x04);
        state.press(0x3f);
        let mut report = [0u8; 8];
        decoder
            .encode(&DecodedReport::Keyboard(state), &mut report)
            .unwrap();
        assert_eq!(report, [0x11, 0, 0x04, 0x3f, 0, 0, 0, 0]);
        assert_eq!(
            decoder.decode(&report),
            Some(DecodedReport::Keyboard(state))
        );
    }

    #[test]
    fn decodes_consumer_array() {
        let descriptor = [
            0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a,
            0xff, 0x03, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        assert_eq!(
            decoder.decode(&[0xe9, 0x00]),
            Some(DecodedReport::Consumer(0x00e9))
        );

        let mut report = [0u8; 2];
        decoder
            .encode(&DecodedReport::Consumer(0x00ea), &mut report)
            .unwrap();
        assert_eq!(report, [0xea, 0]);
    }

    #[test]
    fn encoding_preserves_padding_and_vendor_bits() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, // Mouse
            0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x03,
            0x81, 0x02, // Three buttons
            0x75, 0x05, 0x95, 0x01, 0x81, 0x01, // Five padding bits
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02,
            0x81, 0x06, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        let mut report = [0xf8, 0, 0];
        decoder
            .encode(
                &DecodedReport::Mouse(MouseState {
                    buttons: 0x05,
                    x: 1,
                    y: -1,
                    wheel: 0,
                    pan: 0,
                }),
                &mut report,
            )
            .unwrap();
        assert_eq!(report, [0xfd, 1, 0xff]);
    }

    #[test]
    fn encoding_preserves_buttons_beyond_normalized_eight() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, // Mouse
            0x05, 0x09, 0x19, 0x01, 0x29, 0x0a, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0a,
            0x81, 0x02, // Ten buttons
            0x75, 0x06, 0x95, 0x01, 0x81, 0x01, // Padding
            0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02,
            0x81, 0x06, 0xc0,
        ];
        let decoder = parse_report_descriptor(&descriptor);
        let mut report = [0, 0x01, 0, 0]; // Physical button 9 is vendor-preserved.
        decoder
            .encode(
                &DecodedReport::Mouse(MouseState {
                    buttons: 1,
                    x: 2,
                    y: 3,
                    wheel: 0,
                    pan: 0,
                }),
                &mut report,
            )
            .unwrap();
        assert_eq!(report, [1, 1, 2, 3]);
    }
}

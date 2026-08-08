//! Allocation-free, non-blocking HID macro sequencer.

use crate::usb_host::{DecodedReport, KeyboardState};

pub const MAX_LAYERS: usize = 8;
pub const MAX_PROGRAMS: usize = 16;
pub const MAX_LOOP_DEPTH: usize = 4;
pub const DEFAULT_GAME_SENSITIVITY_MILLI: u16 = 1_000;
pub const MIN_GAME_SENSITIVITY_MILLI: u16 = 100;
pub const MAX_GAME_SENSITIVITY_MILLI: u16 = 10_000;

const EMPTY_STEPS: &[Step] = &[];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Key(u8),
    MouseButton(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mask {
    None,
    Chord,
    Inputs(&'static [Input]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationBehavior {
    Select,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Activation {
    pub chord: &'static [Input],
    pub behavior: ActivationBehavior,
    pub mask: Mask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerBehavior {
    Hold,
    Press,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Trigger {
    pub chord: &'static [Input],
    pub behavior: TriggerBehavior,
    pub mask: Mask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Repeat {
    Once,
    Count(RandomU16),
    WhileActive,
    Forever,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RandomU16 {
    Fixed(u16),
    Uniform { min: u16, max: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RandomI16 {
    Fixed(i16),
    Uniform { min: i16, max: i16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    WaitMs(RandomU16),
    KeyDown(u8),
    KeyUp(u8),
    MouseDown(u8),
    MouseUp(u8),
    MouseMove {
        x: RandomI16,
        y: RandomI16,
    },
    MouseWheel {
        vertical: RandomI16,
        pan: RandomI16,
    },
    Repeat {
        repeat: Repeat,
        steps: &'static [Step],
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Program {
    pub trigger: Trigger,
    pub repeat: Repeat,
    pub steps: &'static [Step],
    pub alternate_steps: Option<&'static [Step]>,
    pub alternate_on_repeat: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layer {
    pub group: u8,
    pub default_enabled: bool,
    pub activation: Option<Activation>,
    pub programs: &'static [Program],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    pub seed: u64,
    pub always_mask: &'static [Input],
    pub layers: &'static [Layer],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputMask {
    modifiers: u8,
    keys: [u8; 14],
    mouse_buttons: u8,
}

impl InputMask {
    const fn empty() -> Self {
        Self {
            modifiers: 0,
            keys: [0; 14],
            mouse_buttons: 0,
        }
    }

    fn add(&mut self, input: Input) {
        match input {
            Input::Key(usage @ 0xe0..=0xe7) => self.modifiers |= 1 << (usage - 0xe0),
            Input::Key(usage @ 0x04..=0x73) => {
                let bit = usize::from(usage - 0x04);
                self.keys[bit / 8] |= 1 << (bit % 8);
            }
            Input::MouseButton(button @ 1..=8) => self.mouse_buttons |= 1 << (button - 1),
            _ => {}
        }
    }

    fn add_spec(&mut self, spec: Mask, chord: &[Input]) {
        let inputs = match spec {
            Mask::None => return,
            Mask::Chord => chord,
            Mask::Inputs(inputs) => inputs,
        };
        for input in inputs.iter().copied() {
            self.add(input);
        }
    }
}

#[derive(Clone, Copy)]
struct Frame {
    steps: &'static [Step],
    index: usize,
    repeat: Repeat,
    completed: u16,
    sampled_count: u16,
}

impl Frame {
    const fn empty() -> Self {
        Self {
            steps: EMPTY_STEPS,
            index: 0,
            repeat: Repeat::Once,
            completed: 0,
            sampled_count: 0,
        }
    }

    const fn new(steps: &'static [Step], repeat: Repeat) -> Self {
        Self {
            steps,
            index: 0,
            repeat,
            completed: 0,
            sampled_count: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct ProgramState {
    trigger_was_down: bool,
    active: bool,
    running: bool,
    waiting: bool,
    wake_at_us: u32,
    owned_buttons: u8,
    owned_keyboard: KeyboardState,
    frames: [Frame; MAX_LOOP_DEPTH],
    depth: usize,
    alternate_next: bool,
}

impl ProgramState {
    const fn new() -> Self {
        Self {
            trigger_was_down: false,
            active: false,
            running: false,
            waiting: false,
            wake_at_us: 0,
            owned_buttons: 0,
            owned_keyboard: KeyboardState::empty(),
            frames: [Frame::empty(); MAX_LOOP_DEPTH],
            depth: 0,
            alternate_next: false,
        }
    }

    fn start(&mut self, program: &Program) {
        self.active = true;
        self.running = true;
        self.waiting = false;
        self.owned_buttons = 0;
        self.owned_keyboard = KeyboardState::empty();
        self.depth = 1;
        let steps = self.next_steps(program);
        self.frames[0] = Frame::new(steps, program.repeat);
    }

    fn next_steps(&mut self, program: &Program) -> &'static [Step] {
        let steps = if self.alternate_next {
            program.alternate_steps.unwrap_or(program.steps)
        } else {
            program.steps
        };
        if program.alternate_steps.is_some() {
            self.alternate_next = !self.alternate_next;
        }
        steps
    }

    fn cancel(&mut self) {
        self.active = false;
        self.running = false;
        self.waiting = false;
        self.owned_buttons = 0;
        self.owned_keyboard = KeyboardState::empty();
        self.depth = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacroMouseReport {
    pub buttons: u8,
    pub x: i16,
    pub y: i16,
    pub wheel: i8,
    pub pan: i8,
}

/// Small deterministic PRNG suitable for timing/position jitter. It is not a
/// cryptographic random-number generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacroRng {
    state: u64,
}

impl MacroRng {
    pub const fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    pub fn reseed(&mut self, entropy: u64) {
        self.state ^= entropy.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let _ = self.next_u32();
    }

    pub fn next_u32(&mut self) -> u32 {
        // xorshift64* has a 2^64-1 period for every nonzero seed.
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        (value.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    pub fn u16(&mut self, value: RandomU16) -> u16 {
        match value {
            RandomU16::Fixed(value) => value,
            RandomU16::Uniform { min, max } => {
                let (min, max) = ordered_u16(min, max);
                min.saturating_add(self.bounded(u32::from(max - min) + 1) as u16)
            }
        }
    }

    pub fn i16(&mut self, value: RandomI16) -> i16 {
        match value {
            RandomI16::Fixed(value) => value,
            RandomI16::Uniform { min, max } => {
                let (min, max) = if min <= max { (min, max) } else { (max, min) };
                let width = (i32::from(max) - i32::from(min) + 1) as u32;
                (i32::from(min) + self.bounded(width) as i32) as i16
            }
        }
    }

    fn bounded(&mut self, bound: u32) -> u32 {
        if bound <= 1 {
            return 0;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next_u32();
            if value >= threshold {
                return value % bound;
            }
        }
    }
}

pub struct MacroEngine {
    config: &'static Config,
    keyboard: KeyboardState,
    physical_mouse_buttons: u8,
    remote_keyboard: KeyboardState,
    remote_mouse_buttons: u8,
    layer_enabled: [bool; MAX_LAYERS],
    activation_was_down: [bool; MAX_LAYERS],
    programs: [ProgramState; MAX_PROGRAMS],
    rng: MacroRng,
    pending_x: i16,
    pending_y: i16,
    pending_wheel: i16,
    pending_pan: i16,
    game_sensitivity_milli: u16,
    sensitivity_remainder_x: i32,
    sensitivity_remainder_y: i32,
    last_queued_buttons: u8,
    last_queued_keyboard: KeyboardState,
}

impl MacroEngine {
    pub fn new(config: &'static Config, runtime_seed: u64) -> Self {
        let mut engine = Self {
            config,
            keyboard: KeyboardState::empty(),
            physical_mouse_buttons: 0,
            remote_keyboard: KeyboardState::empty(),
            remote_mouse_buttons: 0,
            layer_enabled: [false; MAX_LAYERS],
            activation_was_down: [false; MAX_LAYERS],
            programs: [ProgramState::new(); MAX_PROGRAMS],
            rng: MacroRng::new(config.seed ^ runtime_seed),
            pending_x: 0,
            pending_y: 0,
            pending_wheel: 0,
            pending_pan: 0,
            game_sensitivity_milli: DEFAULT_GAME_SENSITIVITY_MILLI,
            sensitivity_remainder_x: 0,
            sensitivity_remainder_y: 0,
            last_queued_buttons: 0,
            last_queued_keyboard: KeyboardState::empty(),
        };
        for (index, layer) in config.layers.iter().take(MAX_LAYERS).enumerate() {
            engine.layer_enabled[index] = layer.default_enabled;
        }
        engine
    }

    /// Set the in-game mouse sensitivity used to scale generated recoil
    /// motion. Physical mouse reports remain untouched. The stored trajectory
    /// is calibrated at 1.000, so generated counts are divided by this value.
    pub fn set_game_sensitivity_milli(&mut self, sensitivity_milli: u16) -> bool {
        if !(MIN_GAME_SENSITIVITY_MILLI..=MAX_GAME_SENSITIVITY_MILLI).contains(&sensitivity_milli) {
            return false;
        }
        self.game_sensitivity_milli = sensitivity_milli;
        self.sensitivity_remainder_x = 0;
        self.sensitivity_remainder_y = 0;
        true
    }

    pub const fn game_sensitivity_milli(&self) -> u16 {
        self.game_sensitivity_milli
    }

    /// Change one remotely-owned HID key without disturbing physical input.
    pub fn set_remote_key(&mut self, usage: u8, pressed: bool) -> bool {
        match usage {
            0x04..=0x73 => {
                if pressed {
                    self.remote_keyboard.press(u16::from(usage));
                } else {
                    self.remote_keyboard.release(u16::from(usage));
                }
            }
            0xe0..=0xe7 => {
                let mask = 1 << (usage - 0xe0);
                if pressed {
                    self.remote_keyboard.modifiers |= mask;
                } else {
                    self.remote_keyboard.modifiers &= !mask;
                }
            }
            _ => return false,
        }
        true
    }

    /// Replace the remotely-owned mouse buttons. Physical buttons are merged.
    pub fn set_remote_mouse_buttons(&mut self, buttons: u8) {
        self.remote_mouse_buttons = buttons;
    }

    /// Queue relative remote-control input at native HID count scale.
    pub fn push_remote_mouse_motion(&mut self, x: i16, y: i16, wheel: i8, pan: i8) {
        self.pending_x = self.pending_x.saturating_add(x);
        self.pending_y = self.pending_y.saturating_add(y);
        self.pending_wheel = self.pending_wheel.saturating_add(i16::from(wheel));
        self.pending_pan = self.pending_pan.saturating_add(i16::from(pan));
    }

    /// Release all remotely-owned inputs. Physical input remains untouched.
    pub fn emergency_release_remote(&mut self) {
        self.remote_keyboard = KeyboardState::empty();
        self.remote_mouse_buttons = 0;
    }

    /// Observe a physical report, update triggers, apply masks, and merge
    /// currently-held synthetic mouse buttons into mouse reports.
    pub fn observe(&mut self, decoded: DecodedReport) -> DecodedReport {
        match decoded {
            DecodedReport::Keyboard(state) => self.keyboard = state,
            DecodedReport::Mouse(state) => self.physical_mouse_buttons = state.buttons,
            DecodedReport::Consumer(_) => {}
        }
        self.evaluate_activations();
        self.evaluate_triggers();
        let mask = self.current_mask();
        match decoded {
            DecodedReport::Keyboard(_) => DecodedReport::Keyboard(self.effective_keyboard(mask)),
            DecodedReport::Mouse(mut state) => {
                state.buttons = self.effective_buttons(mask);
                DecodedReport::Mouse(state)
            }
            value => value,
        }
    }

    pub fn tick(&mut self, now_us: u32) {
        self.evaluate_activations();
        self.evaluate_triggers();
        let mut program_index = 0usize;
        for (layer_index, layer) in self.config.layers.iter().take(MAX_LAYERS).enumerate() {
            for program in layer.programs {
                if program_index == MAX_PROGRAMS {
                    return;
                }
                if !self.layer_enabled[layer_index] {
                    self.programs[program_index].cancel();
                } else {
                    run_program(
                        &mut self.programs[program_index],
                        program,
                        now_us,
                        &mut self.rng,
                        &mut self.pending_x,
                        &mut self.pending_y,
                        &mut self.pending_wheel,
                        &mut self.pending_pan,
                        self.game_sensitivity_milli,
                        &mut self.sensitivity_remainder_x,
                        &mut self.sensitivity_remainder_y,
                    );
                }
                program_index += 1;
            }
        }
    }

    pub fn has_mouse_output(&self) -> bool {
        let buttons = self.effective_buttons(self.current_mask());
        buttons != self.last_queued_buttons
            || self.pending_x != 0
            || self.pending_y != 0
            || self.pending_wheel != 0
            || self.pending_pan != 0
    }

    pub fn has_keyboard_output(&self) -> bool {
        self.effective_keyboard(self.current_mask()) != self.last_queued_keyboard
    }

    pub fn take_keyboard_output(&mut self) -> Option<KeyboardState> {
        if !self.has_keyboard_output() {
            return None;
        }
        let state = self.effective_keyboard(self.current_mask());
        self.last_queued_keyboard = state;
        Some(state)
    }

    /// Move pending relative motion into one report. The caller must retain and
    /// retry that report if its USB endpoint is temporarily busy.
    pub fn take_mouse_output(&mut self) -> Option<MacroMouseReport> {
        if !self.has_mouse_output() {
            return None;
        }
        let report = MacroMouseReport {
            buttons: self.effective_buttons(self.current_mask()),
            x: core::mem::take(&mut self.pending_x),
            y: core::mem::take(&mut self.pending_y),
            wheel: clamp_i8(core::mem::take(&mut self.pending_wheel)),
            pan: clamp_i8(core::mem::take(&mut self.pending_pan)),
        };
        self.last_queued_buttons = report.buttons;
        Some(report)
    }

    fn evaluate_activations(&mut self) {
        for (index, layer) in self.config.layers.iter().take(MAX_LAYERS).enumerate() {
            let Some(activation) = layer.activation else {
                continue;
            };
            let down = self.chord_down(activation.chord);
            if down && !self.activation_was_down[index] {
                match activation.behavior {
                    ActivationBehavior::Select => {
                        for (other_index, other) in
                            self.config.layers.iter().take(MAX_LAYERS).enumerate()
                        {
                            if other.group == layer.group {
                                self.layer_enabled[other_index] = other_index == index;
                            }
                        }
                    }
                    ActivationBehavior::Toggle => {
                        self.layer_enabled[index] = !self.layer_enabled[index];
                    }
                }
            }
            self.activation_was_down[index] = down;
        }
    }

    fn evaluate_triggers(&mut self) {
        let mut program_index = 0usize;
        for (layer_index, layer) in self.config.layers.iter().take(MAX_LAYERS).enumerate() {
            for program in layer.programs {
                if program_index == MAX_PROGRAMS {
                    return;
                }
                let down =
                    self.layer_enabled[layer_index] && self.chord_down(program.trigger.chord);
                let state = &mut self.programs[program_index];
                let rising = down && !state.trigger_was_down;
                match program.trigger.behavior {
                    TriggerBehavior::Hold => {
                        if rising {
                            state.start(program);
                        } else if !down && state.active {
                            state.cancel();
                        }
                    }
                    TriggerBehavior::Press if rising => state.start(program),
                    TriggerBehavior::Toggle if rising => {
                        if state.active {
                            state.cancel();
                        } else {
                            state.start(program);
                        }
                    }
                    _ => {}
                }
                state.trigger_was_down = down;
                program_index += 1;
            }
        }
    }

    fn chord_down(&self, chord: &[Input]) -> bool {
        !chord.is_empty()
            && chord.iter().copied().all(|input| match input {
                Input::Key(usage @ 0xe0..=0xe7) => {
                    (self.keyboard.modifiers | self.remote_keyboard.modifiers)
                        & (1 << (usage - 0xe0))
                        != 0
                }
                Input::Key(usage) => {
                    self.keyboard.is_pressed(u16::from(usage))
                        || self.remote_keyboard.is_pressed(u16::from(usage))
                }
                Input::MouseButton(button @ 1..=8) => {
                    (self.physical_mouse_buttons | self.remote_mouse_buttons) & (1 << (button - 1))
                        != 0
                }
                _ => false,
            })
    }

    fn current_mask(&self) -> InputMask {
        let mut mask = InputMask::empty();
        for input in self.config.always_mask.iter().copied() {
            mask.add(input);
        }
        for (index, layer) in self.config.layers.iter().take(MAX_LAYERS).enumerate() {
            if let Some(activation) = layer.activation
                && self.chord_down(activation.chord)
            {
                mask.add_spec(activation.mask, activation.chord);
            }
            if !self.layer_enabled[index] {
                continue;
            }
            for program in layer.programs {
                if self.chord_down(program.trigger.chord) {
                    mask.add_spec(program.trigger.mask, program.trigger.chord);
                }
            }
        }
        mask
    }

    fn synthetic_buttons(&self) -> u8 {
        self.programs
            .iter()
            .fold(0, |buttons, state| buttons | state.owned_buttons)
    }

    fn effective_keyboard(&self, mask: InputMask) -> KeyboardState {
        let mut keyboard = self.keyboard;
        keyboard.modifiers &= !mask.modifiers;
        for (keys, masked) in keyboard.keys.iter_mut().zip(mask.keys) {
            *keys &= !masked;
        }
        keyboard.modifiers |= self.remote_keyboard.modifiers & !mask.modifiers;
        for ((keys, remote), masked) in keyboard
            .keys
            .iter_mut()
            .zip(self.remote_keyboard.keys)
            .zip(mask.keys)
        {
            *keys |= remote & !masked;
        }
        for state in &self.programs {
            keyboard.modifiers |= state.owned_keyboard.modifiers;
            for (keys, owned) in keyboard.keys.iter_mut().zip(state.owned_keyboard.keys) {
                *keys |= owned;
            }
        }
        keyboard
    }

    fn effective_buttons(&self, mask: InputMask) -> u8 {
        ((self.physical_mouse_buttons | self.remote_mouse_buttons) & !mask.mouse_buttons)
            | self.synthetic_buttons()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_program(
    state: &mut ProgramState,
    program: &Program,
    now_us: u32,
    rng: &mut MacroRng,
    pending_x: &mut i16,
    pending_y: &mut i16,
    pending_wheel: &mut i16,
    pending_pan: &mut i16,
    game_sensitivity_milli: u16,
    sensitivity_remainder_x: &mut i32,
    sensitivity_remainder_y: &mut i32,
) {
    if !state.running || state.depth == 0 {
        return;
    }
    if state.waiting {
        if (now_us.wrapping_sub(state.wake_at_us) as i32) < 0 {
            return;
        }
        state.waiting = false;
    }

    // Bound work per USB frame even if a configuration contains zero-delay
    // infinite loops.
    for _ in 0..64 {
        if state.depth == 0 {
            state.cancel();
            return;
        }
        let frame_index = state.depth - 1;
        if state.frames[frame_index].index >= state.frames[frame_index].steps.len() {
            if repeat_frame(state, frame_index, rng) {
                if frame_index == 0 && program.alternate_on_repeat {
                    state.frames[0].steps = state.next_steps(program);
                }
                continue;
            }
            state.depth -= 1;
            if state.depth == 0 {
                state.active = false;
                state.running = false;
                state.owned_buttons = 0;
                state.owned_keyboard = KeyboardState::empty();
                return;
            }
            continue;
        }
        let step = state.frames[frame_index].steps[state.frames[frame_index].index];
        state.frames[frame_index].index += 1;
        match step {
            Step::WaitMs(value) => {
                let delay_us = u32::from(rng.u16(value)).saturating_mul(1_000);
                if delay_us != 0 {
                    state.waiting = true;
                    state.wake_at_us = now_us.wrapping_add(delay_us);
                    return;
                }
            }
            Step::KeyDown(usage) => set_owned_key(state, usage, true),
            Step::KeyUp(usage) => set_owned_key(state, usage, false),
            Step::MouseDown(buttons) => state.owned_buttons |= buttons,
            Step::MouseUp(buttons) => state.owned_buttons &= !buttons,
            Step::MouseMove { x, y } => {
                let x = scale_for_game_sensitivity(
                    rng.i16(x),
                    game_sensitivity_milli,
                    sensitivity_remainder_x,
                );
                let y = scale_for_game_sensitivity(
                    rng.i16(y),
                    game_sensitivity_milli,
                    sensitivity_remainder_y,
                );
                *pending_x = pending_x.saturating_add(x);
                *pending_y = pending_y.saturating_add(y);
            }
            Step::MouseWheel { vertical, pan } => {
                *pending_wheel = pending_wheel.saturating_add(rng.i16(vertical));
                *pending_pan = pending_pan.saturating_add(rng.i16(pan));
            }
            Step::Repeat { repeat, steps } => {
                if state.depth < MAX_LOOP_DEPTH {
                    state.frames[state.depth] = Frame::new(steps, repeat);
                    state.depth += 1;
                }
            }
        }
    }
}

fn scale_for_game_sensitivity(value: i16, sensitivity_milli: u16, remainder: &mut i32) -> i16 {
    let numerator = i32::from(value)
        .saturating_mul(i32::from(DEFAULT_GAME_SENSITIVITY_MILLI))
        .saturating_add(*remainder);
    let denominator = i32::from(sensitivity_milli.max(1));
    let scaled = numerator / denominator;
    *remainder = numerator % denominator;
    scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn set_owned_key(state: &mut ProgramState, usage: u8, down: bool) {
    if (0xe0..=0xe7).contains(&usage) {
        let mask = 1 << (usage - 0xe0);
        if down {
            state.owned_keyboard.modifiers |= mask;
        } else {
            state.owned_keyboard.modifiers &= !mask;
        }
    } else if down {
        state.owned_keyboard.press(u16::from(usage));
    } else {
        state.owned_keyboard.release(u16::from(usage));
    }
}

fn repeat_frame(state: &mut ProgramState, index: usize, rng: &mut MacroRng) -> bool {
    let frame = &mut state.frames[index];
    frame.completed = frame.completed.saturating_add(1);
    let repeat = match frame.repeat {
        Repeat::Once => false,
        Repeat::Count(count) => {
            if frame.sampled_count == 0 {
                frame.sampled_count = rng.u16(count).max(1);
            }
            frame.completed < frame.sampled_count
        }
        Repeat::WhileActive => state.active,
        Repeat::Forever => true,
    };
    if repeat {
        frame.index = 0;
    }
    repeat
}

const fn ordered_u16(first: u16, second: u16) -> (u16, u16) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn clamp_i8(value: i16) -> i8 {
    value.clamp(i16::from(i8::MIN), i16::from(i8::MAX)) as i8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb_host::MouseState;

    const SIDE5: &[Input] = &[Input::MouseButton(5)];
    const RAPID_STEPS: &[Step] = &[
        Step::MouseDown(1),
        Step::WaitMs(RandomU16::Fixed(10)),
        Step::MouseUp(1),
        Step::WaitMs(RandomU16::Fixed(10)),
    ];
    const PROGRAMS: &[Program] = &[Program {
        trigger: Trigger {
            chord: SIDE5,
            behavior: TriggerBehavior::Hold,
            mask: Mask::Chord,
        },
        repeat: Repeat::WhileActive,
        steps: RAPID_STEPS,
        alternate_steps: None,
        alternate_on_repeat: false,
    }];
    static CONFIG: Config = Config {
        seed: 7,
        always_mask: &[],
        layers: &[Layer {
            group: 0,
            default_enabled: true,
            activation: None,
            programs: PROGRAMS,
        }],
    };

    const SELECT_INPUTS: &[Input] = &[Input::Key(0xe0), Input::Key(0x59)];
    const SELECT_MASK: &[Input] = &[Input::Key(0x59)];
    const MOVE_LOOP: &[Step] = &[Step::MouseMove {
        x: RandomI16::Fixed(0),
        y: RandomI16::Fixed(1),
    }];
    const NESTED_PROGRAMS: &[Program] = &[Program {
        trigger: Trigger {
            chord: &[Input::MouseButton(4)],
            behavior: TriggerBehavior::Press,
            mask: Mask::Chord,
        },
        repeat: Repeat::Once,
        steps: &[Step::Repeat {
            repeat: Repeat::Count(RandomU16::Fixed(3)),
            steps: MOVE_LOOP,
        }],
        alternate_steps: None,
        alternate_on_repeat: false,
    }];
    static LAYER_CONFIG: Config = Config {
        seed: 9,
        always_mask: &[],
        layers: &[
            Layer {
                group: 1,
                default_enabled: true,
                activation: None,
                programs: &[],
            },
            Layer {
                group: 1,
                default_enabled: false,
                activation: Some(Activation {
                    chord: SELECT_INPUTS,
                    behavior: ActivationBehavior::Select,
                    mask: Mask::Inputs(SELECT_MASK),
                }),
                programs: NESTED_PROGRAMS,
            },
        ],
    };

    fn mouse(buttons: u8) -> DecodedReport {
        DecodedReport::Mouse(MouseState {
            buttons,
            x: 0,
            y: 0,
            wheel: 0,
            pan: 0,
        })
    }

    #[test]
    fn held_masked_trigger_runs_and_releases_sequence() {
        let mut engine = MacroEngine::new(&CONFIG, 1);
        assert_eq!(engine.observe(mouse(1 << 4)), mouse(0));
        engine.tick(0);
        assert_eq!(engine.take_mouse_output().unwrap().buttons, 1);
        engine.tick(9_999);
        assert!(!engine.has_mouse_output());
        engine.tick(10_000);
        assert_eq!(engine.take_mouse_output().unwrap().buttons, 0);
        assert_eq!(engine.observe(mouse(0)), mouse(0));
        engine.tick(20_000);
        assert!(!engine.has_mouse_output());
    }

    #[test]
    fn uniform_random_values_stay_inclusive_and_reproducible() {
        let mut first = MacroRng::new(123);
        let mut second = MacroRng::new(123);
        for _ in 0..1_000 {
            let value = first.i16(RandomI16::Uniform { min: -3, max: 4 });
            assert!((-3..=4).contains(&value));
            assert_eq!(value, second.i16(RandomI16::Uniform { min: -3, max: 4 }));
            let delay = first.u16(RandomU16::Uniform { min: 10, max: 20 });
            assert!((10..=20).contains(&delay));
            assert_eq!(delay, second.u16(RandomU16::Uniform { min: 10, max: 20 }));
        }
    }

    #[test]
    fn sensitivity_scaling_preserves_fractional_motion() {
        let mut remainder = 0;
        let scaled: [i16; 3] =
            core::array::from_fn(|_| scale_for_game_sensitivity(1, 1_500, &mut remainder));
        assert_eq!(scaled, [0, 1, 1]);
        assert_eq!(remainder, 0);

        let mut remainder = 0;
        let scaled: [i16; 3] =
            core::array::from_fn(|_| scale_for_game_sensitivity(-1, 1_500, &mut remainder));
        assert_eq!(scaled, [0, -1, -1]);
        assert_eq!(remainder, 0);
    }

    #[test]
    fn sensitivity_setting_is_bounded_and_resets_fractional_state() {
        let mut engine = MacroEngine::new(&CONFIG, 1);
        engine.sensitivity_remainder_x = 12;
        engine.sensitivity_remainder_y = -9;
        assert!(engine.set_game_sensitivity_milli(1_500));
        assert_eq!(engine.game_sensitivity_milli(), 1_500);
        assert_eq!(engine.sensitivity_remainder_x, 0);
        assert_eq!(engine.sensitivity_remainder_y, 0);
        assert!(!engine.set_game_sensitivity_milli(99));
        assert!(!engine.set_game_sensitivity_milli(10_001));
        assert_eq!(engine.game_sensitivity_milli(), 1_500);
    }

    #[test]
    fn remote_input_merges_triggers_motion_and_releases() {
        let mut engine = MacroEngine::new(&CONFIG, 1);
        assert!(engine.set_remote_key(0x1a, true));
        assert!(engine.set_remote_key(0xe1, true));
        assert!(!engine.set_remote_key(0x00, true));
        let keyboard = engine.take_keyboard_output().unwrap();
        assert!(keyboard.is_pressed(0x1a));
        assert_eq!(keyboard.modifiers, 1 << 1);

        engine.set_remote_mouse_buttons(1 << 4);
        engine.push_remote_mouse_motion(12, -7, 1, -1);
        engine.tick(0);
        let mouse = engine.take_mouse_output().unwrap();
        assert_eq!(mouse.buttons, 1);
        assert_eq!((mouse.x, mouse.y, mouse.wheel, mouse.pan), (12, -7, 1, -1));

        engine.emergency_release_remote();
        engine.tick(1_000);
        assert_eq!(engine.take_keyboard_output(), Some(KeyboardState::empty()));
        assert_eq!(engine.take_mouse_output().unwrap().buttons, 0);
    }

    #[test]
    fn checked_in_config_is_only_left_button_recoil() {
        let config = &crate::macro_config::CONFIG;
        assert!(config.always_mask.is_empty());
        assert_eq!(config.layers.len(), 1);
        let layer = &config.layers[0];
        assert!(layer.default_enabled);
        assert_eq!(layer.activation, None);
        assert_eq!(layer.programs.len(), 1);
        let program = &layer.programs[0];
        assert_eq!(program.trigger.chord, &[Input::MouseButton(1)]);
        assert_eq!(program.trigger.behavior, TriggerBehavior::Hold);
        assert_eq!(program.trigger.mask, Mask::None);
        assert_eq!(program.repeat, Repeat::Once);
        assert_eq!(program.alternate_steps, None);
        assert!(!program.alternate_on_repeat);
        assert!(
            program
                .steps
                .iter()
                .all(|step| matches!(step, Step::WaitMs(_) | Step::MouseMove { .. }))
        );

        let mut engine = MacroEngine::new(config, 1);
        assert_eq!(engine.observe(mouse(1)), mouse(1));
    }

    #[test]
    fn layer_selection_masks_keypad_and_nested_loop_counts() {
        let mut engine = MacroEngine::new(&LAYER_CONFIG, 1);
        let mut keyboard = KeyboardState::empty();
        keyboard.modifiers = 1;
        keyboard.press(0x59);
        let DecodedReport::Keyboard(masked) = engine.observe(DecodedReport::Keyboard(keyboard))
        else {
            panic!("not a keyboard report");
        };
        assert_eq!(masked.modifiers, 1);
        assert!(!masked.is_pressed(0x59));

        engine.observe(DecodedReport::Keyboard(KeyboardState::empty()));
        assert_eq!(engine.observe(mouse(1 << 3)), mouse(0));
        engine.tick(1_000);
        let output = engine.take_mouse_output().unwrap();
        assert_eq!((output.x, output.y), (0, 3));
    }

    #[test]
    fn alternate_sequence_changes_on_each_trigger_edge() {
        static ALTERNATE_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::MouseButton(4)],
                        behavior: TriggerBehavior::Press,
                        mask: Mask::None,
                    },
                    repeat: Repeat::Once,
                    steps: &[Step::MouseMove {
                        x: RandomI16::Fixed(-2),
                        y: RandomI16::Fixed(0),
                    }],
                    alternate_steps: Some(&[Step::MouseMove {
                        x: RandomI16::Fixed(2),
                        y: RandomI16::Fixed(0),
                    }]),
                    alternate_on_repeat: false,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&ALTERNATE_CONFIG, 0);
        engine.observe(mouse(1 << 3));
        engine.tick(0);
        assert_eq!(engine.take_mouse_output().unwrap().x, -2);
        engine.observe(mouse(0));
        engine.observe(mouse(1 << 3));
        engine.tick(1_000);
        assert_eq!(engine.take_mouse_output().unwrap().x, 2);
    }

    #[test]
    fn held_program_owns_and_releases_synthetic_keys() {
        static KEY_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::MouseButton(5)],
                        behavior: TriggerBehavior::Hold,
                        mask: Mask::None,
                    },
                    repeat: Repeat::WhileActive,
                    steps: &[
                        Step::KeyDown(0xe0),
                        Step::KeyDown(0x1a),
                        Step::WaitMs(RandomU16::Fixed(20)),
                    ],
                    alternate_steps: None,
                    alternate_on_repeat: false,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&KEY_CONFIG, 0);
        engine.observe(mouse(1 << 4));
        engine.tick(0);
        let keyboard = engine.take_keyboard_output().unwrap();
        assert_eq!(keyboard.modifiers, 1);
        assert!(keyboard.is_pressed(0x1a));

        engine.observe(mouse(0));
        engine.tick(1_000);
        assert_eq!(engine.take_keyboard_output(), Some(KeyboardState::empty()));
    }

    #[test]
    fn alternate_sequence_changes_on_each_program_repeat() {
        static REPEAT_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::MouseButton(4)],
                        behavior: TriggerBehavior::Hold,
                        mask: Mask::None,
                    },
                    repeat: Repeat::WhileActive,
                    steps: &[
                        Step::MouseMove {
                            x: RandomI16::Fixed(-2),
                            y: RandomI16::Fixed(0),
                        },
                        Step::WaitMs(RandomU16::Fixed(1)),
                    ],
                    alternate_steps: Some(&[
                        Step::MouseMove {
                            x: RandomI16::Fixed(2),
                            y: RandomI16::Fixed(0),
                        },
                        Step::WaitMs(RandomU16::Fixed(1)),
                    ]),
                    alternate_on_repeat: true,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&REPEAT_CONFIG, 0);
        engine.observe(mouse(1 << 3));
        engine.tick(0);
        assert_eq!(engine.take_mouse_output().unwrap().x, -2);
        engine.tick(1_000);
        assert_eq!(engine.take_mouse_output().unwrap().x, 2);
        engine.tick(2_000);
        assert_eq!(engine.take_mouse_output().unwrap().x, -2);
    }

    #[test]
    fn repeated_wheel_step_emits_once_per_millisecond() {
        static BURST_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::MouseButton(4)],
                        behavior: TriggerBehavior::Press,
                        mask: Mask::None,
                    },
                    repeat: Repeat::Once,
                    steps: &[Step::Repeat {
                        repeat: Repeat::Count(RandomU16::Fixed(3)),
                        steps: &[
                            Step::MouseWheel {
                                vertical: RandomI16::Fixed(-1),
                                pan: RandomI16::Fixed(0),
                            },
                            Step::WaitMs(RandomU16::Fixed(1)),
                        ],
                    }],
                    alternate_steps: None,
                    alternate_on_repeat: false,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&BURST_CONFIG, 0);
        engine.observe(mouse(1 << 3));
        for now_us in [0, 1_000, 2_000] {
            engine.tick(now_us);
            assert_eq!(engine.take_mouse_output().unwrap().wheel, -1);
        }
        engine.tick(3_000);
        assert!(!engine.has_mouse_output());
    }

    #[test]
    fn random_repeat_count_is_sampled_once_per_loop() {
        static RANDOM_REPEAT_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::MouseButton(4)],
                        behavior: TriggerBehavior::Press,
                        mask: Mask::None,
                    },
                    repeat: Repeat::Once,
                    steps: &[Step::Repeat {
                        repeat: Repeat::Count(RandomU16::Uniform { min: 2, max: 4 }),
                        steps: &[
                            Step::MouseWheel {
                                vertical: RandomI16::Fixed(1),
                                pan: RandomI16::Fixed(0),
                            },
                            Step::WaitMs(RandomU16::Fixed(1)),
                        ],
                    }],
                    alternate_steps: None,
                    alternate_on_repeat: false,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&RANDOM_REPEAT_CONFIG, 0);
        engine.observe(mouse(1 << 3));
        let mut reports = 0;
        for now_us in (0..10_000).step_by(1_000) {
            engine.tick(now_us);
            if let Some(output) = engine.take_mouse_output()
                && output.wheel == 1
            {
                reports += 1;
            }
        }
        assert!((2..=4).contains(&reports));
        assert!(!engine.has_mouse_output());
    }

    #[test]
    fn always_mask_hides_shift_without_breaking_shift_chords() {
        static MASK_CONFIG: Config = Config {
            seed: 1,
            always_mask: &[Input::Key(0xe1)],
            layers: &[Layer {
                group: 0,
                default_enabled: true,
                activation: None,
                programs: &[Program {
                    trigger: Trigger {
                        chord: &[Input::Key(0xe1), Input::Key(0x1a)],
                        behavior: TriggerBehavior::Press,
                        mask: Mask::Inputs(&[Input::Key(0x1a)]),
                    },
                    repeat: Repeat::Once,
                    steps: &[Step::MouseMove {
                        x: RandomI16::Fixed(1),
                        y: RandomI16::Fixed(0),
                    }],
                    alternate_steps: None,
                    alternate_on_repeat: false,
                }],
            }],
        };
        let mut engine = MacroEngine::new(&MASK_CONFIG, 0);
        let mut keyboard = KeyboardState::empty();
        keyboard.modifiers = 1 << 1;
        assert_eq!(
            engine.observe(DecodedReport::Keyboard(keyboard)),
            DecodedReport::Keyboard(KeyboardState::empty())
        );
        keyboard.press(0x1a);
        assert_eq!(
            engine.observe(DecodedReport::Keyboard(keyboard)),
            DecodedReport::Keyboard(KeyboardState::empty())
        );
        engine.tick(0);
        assert_eq!(engine.take_mouse_output().unwrap().x, 1);
    }
}

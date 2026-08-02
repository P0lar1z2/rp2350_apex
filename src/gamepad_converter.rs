//! Allocation-free keyboard/mouse to USB HID gamepad conversion.
//!
//! The converter only maps current physical input state to a gamepad state.
//! It intentionally contains no turbo, recoil compensation, scripted movement,
//! or aim-assistance logic.

use crate::usb_host::{KeyboardState, MouseState};

/// Standard HID gamepad descriptor matching [`GamepadReport::encode`].
///
/// Report layout: four signed 16-bit sticks, 16 buttons, one hat nibble plus
/// padding, then two unsigned 8-bit triggers. There is no Report ID.
pub const GAMEPAD_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x05, // Usage (Game Pad)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x01, //   Usage (Pointer)
    0xa1, 0x00, //   Collection (Physical)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x33, //     Usage (Rx)
    0x09, 0x34, //     Usage (Ry)
    0x16, 0x01, 0x80, // Logical Minimum (-32767)
    0x26, 0xff, 0x7f, // Logical Maximum (32767)
    0x75, 0x10, //     Report Size (16)
    0x95, 0x04, //     Report Count (4)
    0x81, 0x02, //     Input (Data, Variable, Absolute)
    0xc0, //          End Collection
    0x05, 0x09, //   Usage Page (Button)
    0x19, 0x01, //   Usage Minimum (Button 1)
    0x29, 0x10, //   Usage Maximum (Button 16)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x10, //   Report Count (16)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x39, //   Usage (Hat Switch)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x07, //   Logical Maximum (7)
    0x35, 0x00, //   Physical Minimum (0)
    0x46, 0x3b, 0x01, // Physical Maximum (315)
    0x65, 0x14, //   Unit (Degrees)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x42, //   Input (Data, Variable, Absolute, Null State)
    0x65, 0x00, //   Unit (None)
    0x45, 0x00, //   Physical Maximum (0 / unspecified)
    0x75, 0x04, //   Report Size (4)
    0x95, 0x01, //   Report Count (1)
    0x81, 0x03, //   Input (Constant, Variable, Absolute)
    0x09, 0x32, //   Usage (Z / left trigger)
    0x09, 0x35, //   Usage (Rz / right trigger)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xff, 0x00, // Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x02, //   Report Count (2)
    0x81, 0x02, //   Input (Data, Variable, Absolute)
    0xc0, //        End Collection
];

pub const BUTTON_SOUTH: u16 = 1 << 0;
pub const BUTTON_EAST: u16 = 1 << 1;
pub const BUTTON_WEST: u16 = 1 << 2;
pub const BUTTON_NORTH: u16 = 1 << 3;
pub const BUTTON_LEFT_SHOULDER: u16 = 1 << 4;
pub const BUTTON_RIGHT_SHOULDER: u16 = 1 << 5;
pub const BUTTON_VIEW: u16 = 1 << 6;
pub const BUTTON_MENU: u16 = 1 << 7;
pub const BUTTON_LEFT_STICK: u16 = 1 << 8;
pub const BUTTON_RIGHT_STICK: u16 = 1 << 9;

pub const HAT_UP: u8 = 0;
pub const HAT_UP_RIGHT: u8 = 1;
pub const HAT_RIGHT: u8 = 2;
pub const HAT_DOWN_RIGHT: u8 = 3;
pub const HAT_DOWN: u8 = 4;
pub const HAT_DOWN_LEFT: u8 = 5;
pub const HAT_LEFT: u8 = 6;
pub const HAT_UP_LEFT: u8 = 7;
pub const HAT_NEUTRAL: u8 = 8;

const KEY_A: u16 = 0x04;
const KEY_B: u16 = 0x05;
const KEY_C: u16 = 0x06;
const KEY_D: u16 = 0x07;
const KEY_E: u16 = 0x08;
const KEY_G: u16 = 0x0a;
const KEY_H: u16 = 0x0b;
const KEY_M: u16 = 0x10;
const KEY_N: u16 = 0x11;
const KEY_Q: u16 = 0x14;
const KEY_R: u16 = 0x15;
const KEY_S: u16 = 0x16;
const KEY_V: u16 = 0x19;
const KEY_W: u16 = 0x1a;
const KEY_Z: u16 = 0x1d;
const KEY_1: u16 = 0x1e;
const KEY_2: u16 = 0x1f;
const KEY_3: u16 = 0x20;
const KEY_4: u16 = 0x21;
const KEY_ESCAPE: u16 = 0x29;
const KEY_TAB: u16 = 0x2b;
const KEY_SPACE: u16 = 0x2c;

const MODIFIER_CTRL: u8 = (1 << 0) | (1 << 4);
const MODIFIER_SHIFT: u8 = (1 << 1) | (1 << 5);

const MOUSE_LEFT: u8 = 1 << 0;
const MOUSE_RIGHT: u8 = 1 << 1;
const MOUSE_MIDDLE: u8 = 1 << 2;
const MOUSE_BUTTON_4: u8 = 1 << 3;
const MOUSE_BUTTON_5: u8 = 1 << 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GamepadReport {
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub buttons: u16,
    pub hat: u8,
    pub left_trigger: u8,
    pub right_trigger: u8,
}

impl GamepadReport {
    pub const LEN: usize = 13;

    pub const fn neutral() -> Self {
        Self {
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
            buttons: 0,
            hat: HAT_NEUTRAL,
            left_trigger: 0,
            right_trigger: 0,
        }
    }

    pub fn encode(self) -> [u8; Self::LEN] {
        let mut bytes = [0u8; Self::LEN];
        bytes[0..2].copy_from_slice(&self.left_x.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.left_y.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.right_x.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.right_y.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.buttons.to_le_bytes());
        bytes[10] = self.hat & 0x0f;
        bytes[11] = self.left_trigger;
        bytes[12] = self.right_trigger;
        bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConverterConfig {
    /// Stick units added for every mouse count on the horizontal axis.
    pub mouse_gain_x: u16,
    /// Stick units added for every mouse count on the vertical axis.
    pub mouse_gain_y: u16,
    /// Minimum non-zero stick magnitude used to cross a small game deadzone.
    pub mouse_min_axis: u16,
    /// Maximum generated look-stick magnitude.
    pub mouse_max_axis: u16,
    /// Re-center the look stick if the mouse stops producing reports.
    pub mouse_release_us: u32,
    /// Hold controller Y after wheel activity; reports inside one window coalesce.
    pub wheel_button_us: u32,
    pub invert_look_y: bool,
}

/// Conservative starting point for Apex. Final sensitivity and deadzone must be
/// calibrated with the actual mouse DPI and the game's controller settings.
pub const APEX_DEFAULT_CONFIG: ConverterConfig = ConverterConfig {
    mouse_gain_x: 192,
    mouse_gain_y: 192,
    mouse_min_axis: 2_048,
    mouse_max_axis: 32_767,
    mouse_release_us: 2_500,
    wheel_button_us: 30_000,
    invert_look_y: false,
};

pub struct KbmToGamepad {
    config: ConverterConfig,
    keyboard: KeyboardState,
    mouse_buttons: u8,
    pending_mouse_x: i32,
    pending_mouse_y: i32,
    last_mouse_motion_us: u32,
    look_active: bool,
    wheel_button_until_us: u32,
    wheel_button_active: bool,
}

impl KbmToGamepad {
    pub const fn new(config: ConverterConfig) -> Self {
        Self {
            config,
            keyboard: KeyboardState::empty(),
            mouse_buttons: 0,
            pending_mouse_x: 0,
            pending_mouse_y: 0,
            last_mouse_motion_us: 0,
            look_active: false,
            wheel_button_until_us: 0,
            wheel_button_active: false,
        }
    }

    pub fn observe_keyboard(&mut self, keyboard: KeyboardState) {
        self.keyboard = keyboard;
    }

    pub fn observe_mouse(&mut self, mouse: MouseState, now_us: u32) {
        self.mouse_buttons = mouse.buttons;
        if mouse.x != 0 || mouse.y != 0 {
            let y = if self.config.invert_look_y {
                -i32::from(mouse.y)
            } else {
                i32::from(mouse.y)
            };
            self.pending_mouse_x = self.pending_mouse_x.saturating_add(i32::from(mouse.x));
            self.pending_mouse_y = self.pending_mouse_y.saturating_add(y);
            self.last_mouse_motion_us = now_us;
            self.look_active = true;
        }

        if mouse.wheel != 0 {
            self.wheel_button_until_us = now_us.wrapping_add(self.config.wheel_button_us);
            self.wheel_button_active = true;
        }
    }

    pub fn tick(&mut self, now_us: u32) {
        if self.look_active
            && now_us.wrapping_sub(self.last_mouse_motion_us) >= self.config.mouse_release_us
        {
            self.release_look();
        }
        if self.wheel_button_active && deadline_reached(now_us, self.wheel_button_until_us) {
            self.wheel_button_active = false;
        }
    }

    pub fn release_keyboard(&mut self) {
        self.keyboard = KeyboardState::empty();
    }

    pub fn release_mouse(&mut self) {
        self.mouse_buttons = 0;
        self.wheel_button_active = false;
        self.release_look();
    }

    pub fn release_all(&mut self) {
        self.release_keyboard();
        self.release_mouse();
    }

    /// Acknowledge that the current report was accepted by the USB endpoint.
    /// Mouse counts collected for that report can then be discarded. Button
    /// and keyboard state remains level-triggered until physically released.
    pub fn acknowledge_report(&mut self) {
        self.release_look();
    }

    pub fn report(&self) -> GamepadReport {
        let left = movement_axes(self.keyboard);
        let mut buttons = 0u16;
        if key(self.keyboard, KEY_SPACE) {
            buttons |= BUTTON_SOUTH;
        }
        if self.keyboard.modifiers & MODIFIER_CTRL != 0 || key(self.keyboard, KEY_C) {
            buttons |= BUTTON_EAST;
        }
        if key(self.keyboard, KEY_E) || key(self.keyboard, KEY_R) {
            buttons |= BUTTON_WEST;
        }
        if key(self.keyboard, KEY_1)
            || key(self.keyboard, KEY_2)
            || key(self.keyboard, KEY_3)
            || self.wheel_button_active
        {
            buttons |= BUTTON_NORTH;
        }
        if key(self.keyboard, KEY_Q)
            || key(self.keyboard, KEY_Z)
            || self.mouse_buttons & MOUSE_BUTTON_4 != 0
        {
            buttons |= BUTTON_LEFT_SHOULDER;
        }
        if key(self.keyboard, KEY_Z) || self.mouse_buttons & MOUSE_MIDDLE != 0 {
            buttons |= BUTTON_RIGHT_SHOULDER;
        }
        if key(self.keyboard, KEY_M) {
            buttons |= BUTTON_VIEW;
        }
        if key(self.keyboard, KEY_TAB) || key(self.keyboard, KEY_ESCAPE) {
            buttons |= BUTTON_MENU;
        }
        if self.keyboard.modifiers & MODIFIER_SHIFT != 0 {
            buttons |= BUTTON_LEFT_STICK;
        }
        if key(self.keyboard, KEY_V) || self.mouse_buttons & MOUSE_BUTTON_5 != 0 {
            buttons |= BUTTON_RIGHT_STICK;
        }

        GamepadReport {
            left_x: left.0,
            left_y: left.1,
            right_x: scale_mouse_axis(self.pending_mouse_x, self.config.mouse_gain_x, self.config),
            right_y: scale_mouse_axis(self.pending_mouse_y, self.config.mouse_gain_y, self.config),
            buttons,
            hat: hat_switch(self.keyboard),
            left_trigger: if self.mouse_buttons & MOUSE_RIGHT != 0 {
                u8::MAX
            } else {
                0
            },
            right_trigger: if self.mouse_buttons & MOUSE_LEFT != 0 {
                u8::MAX
            } else {
                0
            },
        }
    }

    fn release_look(&mut self) {
        self.pending_mouse_x = 0;
        self.pending_mouse_y = 0;
        self.look_active = false;
    }
}

fn key(state: KeyboardState, usage: u16) -> bool {
    state.is_pressed(usage)
}

fn movement_axes(state: KeyboardState) -> (i16, i16) {
    let horizontal = i8::from(key(state, KEY_D)) - i8::from(key(state, KEY_A));
    let vertical = i8::from(key(state, KEY_S)) - i8::from(key(state, KEY_W));
    let magnitude = if horizontal != 0 && vertical != 0 {
        23_170
    } else {
        32_767
    };
    (
        i16::from(horizontal) * magnitude,
        i16::from(vertical) * magnitude,
    )
}

fn hat_switch(state: KeyboardState) -> u8 {
    let mut up = key(state, KEY_4);
    let mut right = key(state, KEY_G);
    let mut down = key(state, KEY_H);
    let mut left = key(state, KEY_N) || key(state, KEY_B);
    if up && down {
        up = false;
        down = false;
    }
    if left && right {
        left = false;
        right = false;
    }
    match (up, right, down, left) {
        (true, false, false, false) => HAT_UP,
        (true, true, false, false) => HAT_UP_RIGHT,
        (false, true, false, false) => HAT_RIGHT,
        (false, true, true, false) => HAT_DOWN_RIGHT,
        (false, false, true, false) => HAT_DOWN,
        (false, false, true, true) => HAT_DOWN_LEFT,
        (false, false, false, true) => HAT_LEFT,
        (true, false, false, true) => HAT_UP_LEFT,
        _ => HAT_NEUTRAL,
    }
}

fn scale_mouse_axis(delta: i32, gain: u16, config: ConverterConfig) -> i16 {
    if delta == 0 {
        return 0;
    }
    let minimum = i32::from(config.mouse_min_axis.min(32_767));
    let maximum = i32::from(config.mouse_max_axis.min(32_767).max(minimum as u16));
    let magnitude = minimum
        .saturating_add(delta.abs().saturating_mul(i32::from(gain)))
        .min(maximum);
    if delta < 0 {
        -(magnitude as i16)
    } else {
        magnitude as i16
    }
}

fn deadline_reached(now: u32, deadline: u32) -> bool {
    now.wrapping_sub(deadline) < (1 << 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyboard(usages: &[u16], modifiers: u8) -> KeyboardState {
        let mut state = KeyboardState::empty();
        state.modifiers = modifiers;
        for usage in usages {
            state.press(*usage);
        }
        state
    }

    fn mouse(buttons: u8, x: i16, y: i16, wheel: i8) -> MouseState {
        MouseState {
            buttons,
            x,
            y,
            wheel,
            pan: 0,
        }
    }

    #[test]
    fn neutral_report_matches_wire_layout() {
        let report = GamepadReport::neutral();
        assert_eq!(report.encode(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0]);
        assert_eq!(GAMEPAD_REPORT_DESCRIPTOR.last(), Some(&0xc0));
    }

    #[test]
    fn wasd_maps_to_normalized_left_stick_and_opposites_cancel() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_keyboard(keyboard(&[KEY_W, KEY_D], 0));
        assert_eq!(
            (converter.report().left_x, converter.report().left_y),
            (23_170, -23_170)
        );

        converter.observe_keyboard(keyboard(&[KEY_W, KEY_S, KEY_A, KEY_D], 0));
        assert_eq!(
            (converter.report().left_x, converter.report().left_y),
            (0, 0)
        );
    }

    #[test]
    fn apex_action_mapping_has_no_timed_automation() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_keyboard(keyboard(
            &[KEY_SPACE, KEY_C, KEY_E, KEY_1, KEY_Q, KEY_TAB, KEY_M, KEY_V],
            1 << 1,
        ));
        let buttons = converter.report().buttons;
        assert_ne!(buttons & BUTTON_SOUTH, 0);
        assert_ne!(buttons & BUTTON_EAST, 0);
        assert_ne!(buttons & BUTTON_WEST, 0);
        assert_ne!(buttons & BUTTON_NORTH, 0);
        assert_ne!(buttons & BUTTON_LEFT_SHOULDER, 0);
        assert_ne!(buttons & BUTTON_VIEW, 0);
        assert_ne!(buttons & BUTTON_MENU, 0);
        assert_ne!(buttons & BUTTON_LEFT_STICK, 0);
        assert_ne!(buttons & BUTTON_RIGHT_STICK, 0);

        converter.release_keyboard();
        assert_eq!(converter.report().buttons, 0);
    }

    #[test]
    fn mouse_maps_to_triggers_and_bounded_look_axes() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_mouse(mouse(MOUSE_LEFT | MOUSE_RIGHT, 1, -400, 0), 100);
        let report = converter.report();
        assert_eq!(report.left_trigger, 255);
        assert_eq!(report.right_trigger, 255);
        assert_eq!(report.right_x, 2_240);
        assert_eq!(report.right_y, -32_767);
    }

    #[test]
    fn mouse_counts_accumulate_until_the_report_is_accepted() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_mouse(mouse(0, 1, -1, 0), 100);
        converter.observe_mouse(mouse(0, 2, -3, 0), 200);
        assert_eq!(converter.report().right_x, 2_624);
        assert_eq!(converter.report().right_y, -2_816);

        converter.acknowledge_report();
        assert_eq!(converter.report().right_x, 0);
        assert_eq!(converter.report().right_y, 0);
    }

    #[test]
    fn look_stick_recenters_after_timeout_and_detach_releases_buttons() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_mouse(mouse(MOUSE_LEFT, 4, 2, 0), u32::MAX - 1_000);
        converter.tick(1_000);
        assert_ne!(converter.report().right_x, 0);
        converter.tick(1_600);
        assert_eq!(converter.report().right_x, 0);
        assert_eq!(converter.report().right_y, 0);

        converter.release_mouse();
        assert_eq!(converter.report().right_trigger, 0);
    }

    #[test]
    fn wheel_is_one_finite_weapon_cycle_press() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_mouse(mouse(0, 0, 0, 1), 50);
        assert_ne!(converter.report().buttons & BUTTON_NORTH, 0);
        converter.tick(30_049);
        assert_ne!(converter.report().buttons & BUTTON_NORTH, 0);
        converter.tick(30_050);
        assert_eq!(converter.report().buttons & BUTTON_NORTH, 0);
    }

    #[test]
    fn dpad_cardinals_and_diagonal_are_encoded_as_hat_values() {
        let mut converter = KbmToGamepad::new(APEX_DEFAULT_CONFIG);
        converter.observe_keyboard(keyboard(&[KEY_4, KEY_G], 0));
        assert_eq!(converter.report().hat, HAT_UP_RIGHT);
        converter.observe_keyboard(keyboard(&[KEY_N, KEY_G], 0));
        assert_eq!(converter.report().hat, HAT_NEUTRAL);
    }
}

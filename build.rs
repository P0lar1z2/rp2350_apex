//! SPDX-License-Identifier: MIT OR Apache-2.0
//!
//! Copyright (c) 2021–2024 The rp-rs Developers
//! Copyright (c) 2021 rp-rs organization
//! Copyright (c) 2025 Raspberry Pi Ltd.
//!
//! Set up linker scripts

use std::fs::{File, read_to_string};
use std::io::Write;
use std::path::PathBuf;

use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MacroFile {
    version: u8,
    #[serde(default)]
    always_mask: Vec<String>,
    #[serde(default)]
    random: RandomConfig,
    layers: Vec<LayerConfig>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RandomConfig {
    #[serde(default)]
    seed: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerConfig {
    id: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    default: bool,
    activation: Option<ActivationConfig>,
    #[serde(default)]
    macros: Vec<ProgramConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationConfig {
    chord: Vec<String>,
    behavior: String,
    #[serde(default)]
    mask: MaskConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramConfig {
    id: String,
    trigger: TriggerConfig,
    #[serde(default)]
    repeat: RepeatConfig,
    sequence: Vec<StepConfig>,
    #[serde(default)]
    alternate_sequence: Option<Vec<StepConfig>>,
    #[serde(default)]
    alternate_on_repeat: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TriggerConfig {
    chord: Vec<String>,
    behavior: String,
    #[serde(default)]
    mask: MaskConfig,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum MaskConfig {
    #[default]
    Default,
    Name(String),
    Inputs(Vec<String>),
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum RepeatConfig {
    #[default]
    Default,
    Name(String),
    Count(u16),
    Range {
        min: u16,
        max: u16,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum U16Value {
    Fixed(u16),
    Range { min: u16, max: u16 },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum I16Value {
    Fixed(i16),
    Range { min: i16, max: i16 },
}

#[derive(Deserialize)]
#[serde(tag = "do")]
enum StepConfig {
    #[serde(rename = "wait")]
    Wait { ms: U16Value },
    #[serde(rename = "keyboard.key_down")]
    KeyDown { key: String },
    #[serde(rename = "keyboard.key_up")]
    KeyUp { key: String },
    #[serde(rename = "mouse.button_down")]
    MouseDown { button: String },
    #[serde(rename = "mouse.button_up")]
    MouseUp { button: String },
    #[serde(rename = "mouse.move")]
    MouseMove { x: I16Value, y: I16Value },
    #[serde(rename = "mouse.wheel")]
    MouseWheel {
        vertical: I16Value,
        #[serde(default = "zero_i16")]
        pan: I16Value,
    },
    #[serde(rename = "mouse.wheel_burst")]
    MouseWheelBurst {
        vertical: I16Value,
        #[serde(default = "zero_i16")]
        pan: I16Value,
        duration_ms: U16Value,
    },
    #[serde(rename = "repeat")]
    Repeat {
        repeat: RepeatConfig,
        sequence: Vec<StepConfig>,
    },
}

fn zero_i16() -> I16Value {
    I16Value::Fixed(0)
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(rp2040)");
    println!("cargo::rustc-check-cfg=cfg(rp2350)");

    // Put the linker script somewhere the linker can find it
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rustc-link-search={}", out.display());

    compile_macro_config(&out);

    println!("cargo:rerun-if-changed=.pico-rs");
    let contents = read_to_string(".pico-rs")
        .map(|s| s.trim().to_string().to_lowercase())
        .unwrap_or_else(|e| {
            eprintln!("Failed to read file: {}", e);
            String::new()
        });

    // The file `memory.x` is loaded by cortex-m-rt's `link.x` script, which
    // is what we specify in `.cargo/config.toml` for Arm builds
    let target;
    if contents == "rp2040" {
        target = "thumbv6m-none-eabi";
        let memory_x = include_bytes!("rp2040.x");
        let mut f = File::create(out.join("memory.x")).unwrap();
        f.write_all(memory_x).unwrap();
        println!("cargo::rustc-cfg=rp2040");
        println!("cargo:rerun-if-changed=rp2040.x");
    } else {
        if contents.contains("riscv") {
            target = "riscv32imac-unknown-none-elf";
        } else {
            target = "thumbv8m.main-none-eabihf";
        }
        let memory_x = include_bytes!("rp2350.x");
        let mut f = File::create(out.join("memory.x")).unwrap();
        f.write_all(memory_x).unwrap();
        println!("cargo::rustc-cfg=rp2350");
        println!("cargo:rerun-if-changed=rp2350.x");
    }

    let re = Regex::new(r"target = .*").unwrap();
    let config_toml = include_str!(".cargo/config.toml");
    let result = re.replace(config_toml, format!("target = \"{}\"", target));
    let mut f = File::create(".cargo/config.toml").unwrap();
    f.write_all(result.as_bytes()).unwrap();

    // The file `rp2350_riscv.x` is what we specify in `.cargo/config.toml` for
    // RISC-V builds
    let rp2350_riscv_x = include_bytes!("rp2350_riscv.x");
    let mut f = File::create(out.join("rp2350_riscv.x")).unwrap();
    f.write_all(rp2350_riscv_x).unwrap();
    println!("cargo:rerun-if-changed=rp2350_riscv.x");

    println!("cargo:rerun-if-changed=build.rs");
}

fn compile_macro_config(out: &std::path::Path) {
    const PATH: &str = "macro_config.toml";
    println!("cargo:rerun-if-changed={PATH}");
    let source =
        read_to_string(PATH).unwrap_or_else(|error| panic!("failed to read {PATH}: {error}"));
    let config: MacroFile =
        toml::from_str(&source).unwrap_or_else(|error| panic!("invalid {PATH}: {error}"));
    assert_eq!(config.version, 1, "unsupported macro config version");
    assert!(
        config.layers.len() <= 8,
        "at most 8 macro layers are supported"
    );
    let program_count: usize = config.layers.iter().map(|layer| layer.macros.len()).sum();
    assert!(
        program_count <= 16,
        "at most 16 macro programs are supported"
    );

    let mut groups = Vec::<String>::new();
    for layer in &config.layers {
        let group = if layer.group.is_empty() {
            &layer.id
        } else {
            &layer.group
        };
        if !groups.contains(group) {
            groups.push(group.clone());
        }
    }

    let mut generated = String::from(
        "// @generated by build.rs from macro_config.toml\nuse crate::macro_engine::*;\n",
    );
    generated.push_str(&format!(
        "pub static CONFIG: Config = Config {{ seed: {}u64, always_mask: &[",
        config.random.seed
    ));
    write_inputs_allow_empty(&mut generated, &config.always_mask, "always_mask");
    generated.push_str("], layers: &[\n");
    for layer in &config.layers {
        let group_name = if layer.group.is_empty() {
            &layer.id
        } else {
            &layer.group
        };
        let group = groups.iter().position(|value| value == group_name).unwrap();
        generated.push_str(&format!(
            "Layer {{ group: {group}u8, default_enabled: {}, activation: ",
            layer.default
        ));
        match &layer.activation {
            Some(activation) => {
                generated.push_str("Some(Activation { chord: &[");
                write_inputs(&mut generated, &activation.chord, &layer.id);
                generated.push_str("], behavior: ");
                generated.push_str(match activation.behavior.as_str() {
                    "select" => "ActivationBehavior::Select",
                    "toggle" => "ActivationBehavior::Toggle",
                    other => panic!("layer {} has invalid activation behavior {other}", layer.id),
                });
                generated.push_str(", mask: ");
                write_mask(
                    &mut generated,
                    &activation.mask,
                    &activation.chord,
                    &layer.id,
                );
                generated.push_str(" })");
            }
            None => generated.push_str("None"),
        }
        generated.push_str(", programs: &[");
        for program in &layer.macros {
            assert!(
                !program.sequence.is_empty(),
                "macro {} has an empty sequence",
                program.id
            );
            generated.push_str("Program { trigger: Trigger { chord: &[");
            write_inputs(&mut generated, &program.trigger.chord, &program.id);
            generated.push_str("], behavior: ");
            generated.push_str(match program.trigger.behavior.as_str() {
                "hold" => "TriggerBehavior::Hold",
                "press" => "TriggerBehavior::Press",
                "toggle" => "TriggerBehavior::Toggle",
                other => panic!("macro {} has invalid trigger behavior {other}", program.id),
            });
            generated.push_str(", mask: ");
            write_mask(
                &mut generated,
                &program.trigger.mask,
                &program.trigger.chord,
                &program.id,
            );
            generated.push_str(" }, repeat: ");
            write_repeat(&mut generated, &program.repeat, &program.id);
            generated.push_str(", steps: &[");
            write_steps(&mut generated, &program.sequence, &program.id, 0);
            generated.push_str("], alternate_steps: ");
            if let Some(alternate) = &program.alternate_sequence {
                assert!(
                    !alternate.is_empty(),
                    "macro {} has an empty alternate_sequence",
                    program.id
                );
                generated.push_str("Some(&[");
                write_steps(&mut generated, alternate, &program.id, 0);
                generated.push_str("])");
            } else {
                generated.push_str("None");
            }
            generated.push_str(&format!(
                ", alternate_on_repeat: {} }},",
                program.alternate_on_repeat
            ));
        }
        generated.push_str("] },\n");
    }
    generated.push_str("] };\n");
    std::fs::write(out.join("macro_config.rs"), generated)
        .expect("failed to write generated macro configuration");
}

fn write_inputs(output: &mut String, inputs: &[String], context: &str) {
    assert!(!inputs.is_empty(), "{context} has an empty chord");
    write_inputs_allow_empty(output, inputs, context);
}

fn write_inputs_allow_empty(output: &mut String, inputs: &[String], context: &str) {
    for input in inputs {
        output.push_str(
            &parse_input(input).unwrap_or_else(|| panic!("{context}: unknown input {input}")),
        );
        output.push(',');
    }
}

fn write_mask(output: &mut String, mask: &MaskConfig, chord: &[String], context: &str) {
    match mask {
        MaskConfig::Default => output.push_str("Mask::None"),
        MaskConfig::Name(name) if name == "none" => output.push_str("Mask::None"),
        MaskConfig::Name(name) if name == "chord" => output.push_str("Mask::Chord"),
        MaskConfig::Name(name) => panic!("{context}: invalid mask {name}"),
        MaskConfig::Inputs(inputs) => {
            output.push_str("Mask::Inputs(&[");
            write_inputs(output, inputs, context);
            output.push_str("])");
        }
    }
    let _ = chord;
}

fn write_repeat(output: &mut String, repeat: &RepeatConfig, context: &str) {
    match repeat {
        RepeatConfig::Default => output.push_str("Repeat::Once"),
        RepeatConfig::Count(count) => {
            assert!(*count != 0, "{context}: repeat count must be positive");
            output.push_str(&format!("Repeat::Count(RandomU16::Fixed({count}))"));
        }
        RepeatConfig::Range { min, max } => {
            assert!(*min != 0, "{context}: repeat range must be positive");
            assert!(
                min <= max,
                "{context}: repeat range has min greater than max"
            );
            output.push_str(&format!(
                "Repeat::Count(RandomU16::Uniform {{ min: {min}, max: {max} }})"
            ));
        }
        RepeatConfig::Name(name) => output.push_str(match name.as_str() {
            "once" => "Repeat::Once",
            "while_active" => "Repeat::WhileActive",
            "forever" => "Repeat::Forever",
            other => panic!("{context}: invalid repeat value {other}"),
        }),
    }
}

fn write_steps(output: &mut String, steps: &[StepConfig], context: &str, depth: usize) {
    assert!(depth < 4, "{context}: loop nesting exceeds 4 levels");
    for step in steps {
        match step {
            StepConfig::Wait { ms } => {
                output.push_str("Step::WaitMs(");
                write_u16(output, ms);
                output.push_str("),");
            }
            StepConfig::KeyDown { key } => output.push_str(&format!(
                "Step::KeyDown({}),",
                keyboard_usage(key)
                    .unwrap_or_else(|| panic!("{context}: unknown keyboard key {key}"))
            )),
            StepConfig::KeyUp { key } => output.push_str(&format!(
                "Step::KeyUp({}),",
                keyboard_usage(key)
                    .unwrap_or_else(|| panic!("{context}: unknown keyboard key {key}"))
            )),
            StepConfig::MouseDown { button } => output.push_str(&format!(
                "Step::MouseDown({}),",
                mouse_button_mask(button)
                    .unwrap_or_else(|| panic!("{context}: unknown mouse button {button}"))
            )),
            StepConfig::MouseUp { button } => output.push_str(&format!(
                "Step::MouseUp({}),",
                mouse_button_mask(button)
                    .unwrap_or_else(|| panic!("{context}: unknown mouse button {button}"))
            )),
            StepConfig::MouseMove { x, y } => {
                output.push_str("Step::MouseMove { x: ");
                write_i16(output, x);
                output.push_str(", y: ");
                write_i16(output, y);
                output.push_str(" },");
            }
            StepConfig::MouseWheel { vertical, pan } => {
                output.push_str("Step::MouseWheel { vertical: ");
                write_i16(output, vertical);
                output.push_str(", pan: ");
                write_i16(output, pan);
                output.push_str(" },");
            }
            StepConfig::MouseWheelBurst {
                vertical,
                pan,
                duration_ms,
            } => {
                validate_positive_u16(duration_ms, context, "wheel burst duration");
                output.push_str("Step::Repeat { repeat: Repeat::Count(");
                write_u16(output, duration_ms);
                output.push_str("), steps: &[Step::MouseWheel { vertical: ");
                write_i16(output, vertical);
                output.push_str(", pan: ");
                write_i16(output, pan);
                output.push_str(" },Step::WaitMs(RandomU16::Fixed(1)),] },");
            }
            StepConfig::Repeat { repeat, sequence } => {
                assert!(
                    !sequence.is_empty(),
                    "{context}: repeat has an empty sequence"
                );
                output.push_str("Step::Repeat { repeat: ");
                write_repeat(output, repeat, context);
                output.push_str(", steps: &[");
                write_steps(output, sequence, context, depth + 1);
                output.push_str("] },");
            }
        }
    }
}

fn write_u16(output: &mut String, value: &U16Value) {
    match value {
        U16Value::Fixed(value) => output.push_str(&format!("RandomU16::Fixed({value})")),
        U16Value::Range { min, max } => {
            assert!(min <= max, "random u16 range has min greater than max");
            output.push_str(&format!("RandomU16::Uniform {{ min: {min}, max: {max} }}"))
        }
    }
}

fn validate_positive_u16(value: &U16Value, context: &str, name: &str) {
    match value {
        U16Value::Fixed(value) => assert!(*value != 0, "{context}: {name} must be positive"),
        U16Value::Range { min, max } => {
            assert!(*min != 0, "{context}: {name} range must be positive");
            assert!(
                min <= max,
                "{context}: {name} range has min greater than max"
            );
        }
    }
}

fn write_i16(output: &mut String, value: &I16Value) {
    match value {
        I16Value::Fixed(value) => output.push_str(&format!("RandomI16::Fixed({value})")),
        I16Value::Range { min, max } => {
            assert!(min <= max, "random i16 range has min greater than max");
            output.push_str(&format!("RandomI16::Uniform {{ min: {min}, max: {max} }}"))
        }
    }
}

fn parse_input(value: &str) -> Option<String> {
    if let Some(usage) = keyboard_usage(value) {
        return Some(format!("Input::Key({usage})"));
    }
    let button = mouse_button_number(value)?;
    Some(format!("Input::MouseButton({button})"))
}

fn keyboard_usage(value: &str) -> Option<u8> {
    let usage = match value {
        "key.left_ctrl" => 0xe0,
        "key.left_shift" => 0xe1,
        "key.left_alt" => 0xe2,
        "key.left_gui" => 0xe3,
        "key.right_ctrl" => 0xe4,
        "key.right_shift" => 0xe5,
        "key.right_alt" => 0xe6,
        "key.right_gui" => 0xe7,
        "key.enter" => 0x28,
        "key.escape" => 0x29,
        "key.tab" => 0x2b,
        "key.space" => 0x2c,
        "key.print_screen" => 0x46,
        "key.scroll_lock" => 0x47,
        "key.pause" => 0x48,
        "key.insert" => 0x49,
        "key.home" => 0x4a,
        "key.page_up" => 0x4b,
        "key.delete" => 0x4c,
        "key.end" => 0x4d,
        "key.page_down" => 0x4e,
        "key.right" => 0x4f,
        "key.left" => 0x50,
        "key.down" => 0x51,
        "key.up" => 0x52,
        "key.num_lock" => 0x53,
        "key.kp_divide" => 0x54,
        "key.kp_multiply" => 0x55,
        "key.kp_subtract" => 0x56,
        "key.kp_add" => 0x57,
        "key.kp_enter" => 0x58,
        "key.kp_1" => 0x59,
        "key.kp_2" => 0x5a,
        "key.kp_3" => 0x5b,
        "key.kp_4" => 0x5c,
        "key.kp_5" => 0x5d,
        "key.kp_6" => 0x5e,
        "key.kp_7" => 0x5f,
        "key.kp_8" => 0x60,
        "key.kp_9" => 0x61,
        "key.kp_0" => 0x62,
        _ if single_key_usage(value).is_some() => single_key_usage(value).unwrap(),
        _ if function_key_usage(value).is_some() => function_key_usage(value).unwrap(),
        _ => return None,
    };
    Some(usage)
}

fn single_key_usage(value: &str) -> Option<u8> {
    if value.len() != 5 || !value.starts_with("key.") {
        return None;
    }
    match value.as_bytes()[4] {
        letter @ b'a'..=b'z' => Some(letter - b'a' + 0x04),
        digit @ b'1'..=b'9' => Some(digit - b'1' + 0x1e),
        b'0' => Some(0x27),
        _ => None,
    }
}

fn function_key_usage(value: &str) -> Option<u8> {
    let number = value.strip_prefix("key.f")?.parse::<u8>().ok()?;
    match number {
        1..=12 => Some(0x3a + number - 1),
        13..=24 => Some(0x68 + number - 13),
        _ => None,
    }
}

fn mouse_button_number(value: &str) -> Option<u8> {
    match value {
        "mouse.left" | "mouse.button1" | "left" => Some(1),
        "mouse.right" | "mouse.button2" | "right" => Some(2),
        "mouse.middle" | "mouse.button3" | "middle" => Some(3),
        "mouse.button4" | "button4" => Some(4),
        "mouse.button5" | "button5" => Some(5),
        "mouse.button6" | "button6" => Some(6),
        "mouse.button7" | "button7" => Some(7),
        "mouse.button8" | "button8" => Some(8),
        _ => None,
    }
}

fn mouse_button_mask(value: &str) -> Option<u8> {
    mouse_button_number(value).map(|button| 1 << (button - 1))
}

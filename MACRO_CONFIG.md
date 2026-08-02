# Mouse macro configuration

`macro_config.toml` is validated and compiled into fixed Rust tables by
`build.rs`. The firmware does not parse TOML at runtime and does not allocate
memory for scripts.

## Layers and switching

Layers in the same `group` are mutually exclusive when their activation uses
`behavior = "select"`. A `toggle` activation independently enables or disables
its layer.

```toml
[[layers]]
id = "rapid_fire"
group = "firing_mode"

[layers.activation]
chord = ["key.left_ctrl", "key.kp_1"]
behavior = "select"
mask = ["key.kp_1"]
```

At most 8 layers and 16 programs are supported. Exactly one layer in a select
group should normally have `default = true`.

## Triggers and masks

Inputs listed at the top level are always hidden from the computer while still
remaining available to chord detection:

```toml
always_mask = ["key.left_shift"]
```

```toml
[layers.macros.trigger]
chord = ["mouse.button5"]
behavior = "hold"
mask = "chord"
```

Trigger behaviors:

- `hold`: run while the complete chord remains held; release cancels it.
- `press`: run once on the chord's rising edge.
- `toggle`: each rising edge starts or cancels the program.

Mask values:

- `"none"`: forward every physical input.
- `"chord"`: consume every input in the trigger chord.
- `[...]`: consume only the listed inputs.

A multi-key chord is masked after the complete chord matches. For example, if
Ctrl is pressed before keypad 1, the initial Ctrl press can already have
reached the computer. Masking only the keypad key avoids delaying normal Ctrl
input.

## Sequence and loops

The program-level `repeat` accepts `"once"`, `"while_active"`, `"forever"`, a
positive integer, or an inclusive random range. A local loop uses the same
values:

```toml
sequence = [
  { do = "repeat", repeat = 3, sequence = [
    { do = "mouse.move", x = 0, y = 2 },
    { do = "wait", ms = 12 },
  ] },
]
```

```toml
repeat = { min = 2, max = 4 }
sequence = [
  { do = "repeat", repeat = { min = 3, max = 6 }, sequence = [
    { do = "mouse.move", x = 0, y = 2 },
  ] },
]
```

A random repeat count is sampled once when that loop runs, not once per step.

An optional `startup_sequence` runs once on each trigger start before the main
`sequence`. Keys and mouse buttons pressed by it remain owned by the program,
but the startup steps are not replayed by program-level repeats:

```toml
startup_sequence = [
  { do = "keyboard.key_down", key = "key.left_ctrl" },
  { do = "wait", ms = 100 },
]
sequence = [{ do = "mouse.wheel", vertical = -1 }]
```

When `alternate_sequence` is present, trigger edges alternate between
`sequence` and `alternate_sequence`, starting with `sequence`:

```toml
sequence = [{ do = "mouse.move", x = -1, y = 0 }]
alternate_sequence = [{ do = "mouse.move", x = 1, y = 0 }]
```

Set `alternate_on_repeat = true` to alternate on every program-level repeat as
well. This is useful with `repeat = "while_active"` for continuously replayed
left/right sequences.

Loops may be nested up to three local levels (four runtime frames including
the program sequence). The executor performs at most 64 zero-delay operations
per USB frame, so an accidental zero-delay infinite loop cannot block USB.

Available steps:

```toml
{ do = "wait", ms = 20 }
{ do = "keyboard.key_down", key = "key.w" }
{ do = "keyboard.key_up", key = "key.w" }
{ do = "mouse.button_down", button = "left" }
{ do = "mouse.button_up", button = "left" }
{ do = "mouse.move", x = -1, y = 3 }
{ do = "mouse.wheel", vertical = 1, pan = 0 }
{ do = "mouse.wheel_burst", vertical = -1, duration_ms = { min = 7, max = 10 } }
```

`mouse.wheel` emits one wheel delta. `mouse.wheel_burst` emits the delta once
per millisecond for `duration_ms`; its fixed value or range must be positive.
The burst duration is sampled once at the start of each burst. The upstream
HID interrupt endpoint advertises a 1 ms poll interval so these reports can be
consumed separately by the host.

## Timing adjustment

The executor runs on a 1 ms scheduler. A wait range such as
`ms = { min = 45, max = 55 }` is sampled once each time that step is reached;
the following key transition occurs after the sampled delay. To slow a macro,
increase those wait ranges. To make wheel input easier to recognize without
changing the gaps between key states, increase only `duration_ms`.

As a frame-time reference, one rendered frame is about 6.94 ms at 144 Hz,
8.33 ms at 120 Hz, or 16.67 ms at 60 Hz. USB reports and game input sampling
are asynchronous, so validate final values from a HID event trace or training
mode rather than assuming a report always lands in a particular game frame.

The included Shift+W preset is currently a stage-one isolation test. It assumes
W is already held before Shift starts the macro, transfers W to synthetic
ownership without releasing it, and presses Ctrl. After a fixed 120 ms slide
setup it jumps with an 80 ms wheel-down burst, then 120 ms after jump onset
presses A while W remains held. W+A overlap for 100 ms, then W is released and
A remains held for another 100 ms. Wheel-forward is emitted continuously across
both phases of the 200 ms A hold. A is released 320 ms after jump onset and is
the final direction press inside the 400 ms post-jump lurch window. All later
bunny-hops are exactly 680 ms apart with no direction key held; only Ctrl and
80 ms wheel-down bursts remain. It never presses D or S. This deliberately
isolates a one-time leftward lurch followed by directionless momentum-preserving
bunny-hops.

Buttons are `left`, `right`, `middle`, and `button4` through `button8`.
Program cancellation releases only buttons owned by that program; physical
buttons and buttons held by another program remain pressed.

## Random values

Every numeric action parameter can be fixed or uniformly randomized over an
inclusive range:

```toml
{ do = "wait", ms = { min = 16, max = 22 } }
{ do = "mouse.move", x = { min = -1, max = 1 }, y = { min = 1, max = 3 } }
{ do = "mouse.wheel_burst", vertical = -1, duration_ms = { min = 7, max = 10 } }
{ do = "repeat", repeat = { min = 2, max = 4 }, sequence = [
  { do = "mouse.wheel", vertical = 1 },
] }
```

The generator is deterministic and non-cryptographic. `random.seed` is mixed
with the RP2350 timer and USB vendor ID at boot. Set a nonzero seed for a
repeatable base sequence, or leave it at zero for timer-dependent sequences.

## Input names

Supported keyboard names include:

- `key.a` through `key.z`, `key.0` through `key.9`, and `key.f1` through
  `key.f24`;
- left/right Ctrl, Shift, Alt and GUI, for example `key.left_ctrl`;
- `key.enter`, `key.escape`, `key.tab`, `key.space`;
- arrow/navigation keys and `key.print_screen`, `key.scroll_lock`, `key.pause`;
- `key.kp_0` through `key.kp_9`, keypad Enter and keypad operators.

Mouse inputs are `mouse.left`, `mouse.right`, `mouse.middle`, or
`mouse.button1` through `mouse.button8`.

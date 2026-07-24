//! SPDX-License-Identifier: MIT OR Apache-2.0
//!
//! Copyright (c) 2021–2024 The rp-rs Developers
//! Copyright (c) 2021 rp-rs organization
//! Copyright (c) 2025 Raspberry Pi Ltd.
//!
//! # GPIO 'Blinky' Example
//!
//! This application demonstrates how to control a GPIO pin on the rp2040 and rp235x.
//!
//! It may need to be adapted to your particular board layout and/or pin assignment.

#![no_std]
#![no_main]

#[cfg(rp2350)]
mod dynamic_clone;
#[cfg(rp2350)]
mod pio_host;

use defmt::*;
use defmt_rtt as _;
#[cfg(rp2040)]
use embedded_hal::delay::DelayNs;
#[cfg(rp2350)]
use embedded_hal::digital::InputPin;
#[cfg(rp2040)]
use embedded_hal::digital::OutputPin;
#[cfg(target_arch = "riscv32")]
use panic_halt as _;
#[cfg(target_arch = "arm")]
use panic_probe as _;

// Alias for our HAL crate
use hal::entry;

#[cfg(rp2350)]
use rp235x_hal as hal;

#[cfg(rp2040)]
use rp2040_hal as hal;

#[cfg(rp2350)]
use hal::{
    clocks::Clock,
    gpio::{FunctionPio0, InputOverride, OutputDriveStrength, OutputSlewRate},
    pio::{Buffers, PIOBuilder, PIOExt, PinDir, ShiftDirection},
};
#[cfg(rp2350)]
use pio::{
    Instruction, InstructionOperands, MovDestination, MovOperation, MovSource, SetDestination,
};
#[cfg(rp2350)]
use pio_host::{BusSpeed, PioUsbHost, TransactionError};
#[cfg(rp2350)]
use t2::usb_host::{
    CloneIdentity, CloneProfile, CloneString, SetupPacket, parse_clone_configuration,
};
#[cfg(rp2350)]
use usb_device::{
    bus::UsbBusAllocator,
    device::{StringDescriptors, UsbDeviceBuilder, UsbRev, UsbVidPid},
};

// use bsp::entry;
// use bsp::hal;
// use rp_pico as bsp;

/// The linker will place this boot block at the start of our program image. We
/// need this to help the ROM bootloader get our code up and running.
/// Note: This boot block is not necessary when using a rp-hal based BSP
/// as the BSPs already perform this step.
#[unsafe(link_section = ".boot2")]
#[used]
#[cfg(rp2040)]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;

/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
#[cfg(rp2350)]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

/// External high-speed crystal on the Raspberry Pi Pico 2 board is 12 MHz.
/// Adjust if your board has a different frequency
const XTAL_FREQ_HZ: u32 = 12_000_000u32;

/// Entry point to our bare-metal application.
///
/// The `#[hal::entry]` macro ensures the Cortex-M start-up code calls this function
/// as soon as all global variables and the spinlock are initialised.
///
/// The function configures the rp2040 and rp235x peripherals, then toggles a GPIO pin in
/// an infinite loop. If there is an LED connected to that pin, it will blink.
#[entry]
fn main() -> ! {
    info!("Program start");
    // Grab our singleton objects
    let mut pac = hal::pac::Peripherals::take().unwrap();

    // Set up the watchdog driver - needed by the clock setup code
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);

    // Configure the clocks. USB PIO needs clk_sys to be an exact multiple of
    // 12 MHz; 144 MHz gives exact /3 TX and /1.5 RX clock divisors.
    #[cfg(rp2040)]
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap();

    #[cfg(rp2350)]
    let clocks = {
        use hal::{
            clocks::ClocksManager,
            fugit::RateExtU32,
            pll::{PLLConfig, common_configs::PLL_USB_48MHZ, setup_pll_blocking},
            xosc::setup_xosc_blocking,
        };

        const PLL_SYS_144MHZ: PLLConfig = PLLConfig {
            vco_freq: hal::fugit::HertzU32::MHz(1440),
            refdiv: 1,
            post_div1: 5,
            post_div2: 2,
        };

        let xosc = setup_xosc_blocking(pac.XOSC, XTAL_FREQ_HZ.Hz()).unwrap();
        watchdog.enable_tick_generation((XTAL_FREQ_HZ / 1_000_000) as u16);
        let mut clocks = ClocksManager::new(pac.CLOCKS);
        let pll_sys = setup_pll_blocking(
            pac.PLL_SYS,
            xosc.operating_frequency(),
            PLL_SYS_144MHZ,
            &mut clocks,
            &mut pac.RESETS,
        )
        .unwrap();
        let pll_usb = setup_pll_blocking(
            pac.PLL_USB,
            xosc.operating_frequency(),
            PLL_USB_48MHZ,
            &mut clocks,
            &mut pac.RESETS,
        )
        .unwrap();
        clocks.init_default(&xosc, &pll_sys, &pll_usb).unwrap();
        clocks
    };

    #[cfg(rp2040)]
    let mut timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);

    #[cfg(rp2350)]
    let mut timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);

    // The single-cycle I/O block controls our GPIO pins
    let sio = hal::Sio::new(pac.SIO);

    // Set the pins to their default state
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    #[cfg(rp2040)]
    {
        // Raspberry Pi Pico/Pico 2 use a regular LED on GPIO25.
        let mut led_pin = pins.gpio25.into_push_pull_output();
        loop {
            info!("on!");
            led_pin.set_high().unwrap();
            timer.delay_ms(200);
            info!("off!");
            led_pin.set_low().unwrap();
            timer.delay_ms(200);
        }
    }

    #[cfg(rp2350)]
    {
        // The Waveshare RP2350-USB-A has a WS2812B RGB LED on GPIO16.
        // A WS2812 needs an 800 kHz encoded data stream, so a regular GPIO
        // high/low blink is not sufficient. PIO generates the required timing.
        const WS2812_PIN: u8 = 16;
        const WS2812_SM_HZ: u64 = 8_000_000;

        const USB_DP_PIN: u8 = 12;
        const USB_DM_PIN: u8 = 13;

        let _ws2812_pin = pins.gpio16.into_function::<FunctionPio0>();

        // USB-A is D+=GP12, D-=GP13. Both pins remain readable while PIO0 owns
        // their output function; PIO1 will be used as the input-only receiver.
        // A USB host port must terminate both data lines with pull-downs
        // (Pico-PIO-USB calls gpio_pull_down on both); without them D- floats
        // and the device sees an undefined line state.
        let mut usb_dp = pins
            .gpio12
            .into_function::<FunctionPio0>()
            .into_pull_type::<hal::gpio::PullDown>();
        let mut usb_dm = pins
            .gpio13
            .into_function::<FunctionPio0>()
            .into_pull_type::<hal::gpio::PullDown>();
        usb_dm.set_drive_strength(OutputDriveStrength::TwelveMilliAmps);
        usb_dm.set_slew_rate(OutputSlewRate::Fast);
        usb_dm.set_input_enable(true);
        usb_dm.set_input_override(InputOverride::Invert);
        usb_dp.set_drive_strength(OutputDriveStrength::TwelveMilliAmps);
        usb_dp.set_slew_rate(OutputSlewRate::Fast);
        usb_dp.set_input_enable(true);
        // The RX PIO program is deliberately written for inverted inputs,
        // matching its edge/EOP state machine.
        usb_dp.set_input_override(InputOverride::Invert);

        let (mut pio, sm0, sm1, _, _) = pac.PIO0.split(&mut pac.RESETS);

        // Full-speed USB transmitter, using Pico-PIO-USB's proven design: the
        // CPU pre-encodes NRZI + bit stuffing into a stream of 2-bit symbols
        // and the state machine simply jumps between four line states. An
        // underrun stalls `out pc` harmlessly instead of terminating the
        // packet, which the previous on-the-fly encoder did whenever autopull
        // missed a byte boundary. `out pc, 2` can only reach addresses 0-3,
        // so this program MUST be installed first, at origin 0.
        //
        // Symbols: 0 = EOP (2 bit times SE0, then one driven J via address
        // 1's fall-through), 1 = J bit, 2 = release bus, 3 = K bit. Pin order
        // is DP,DM on this board, hence J = 0b01 for full-speed.
        let usb_tx_program = pio_proc::pio_asm!(
            ".origin 0",
            ".side_set 2",
            "irq 1 side 0b00 [7]",
            ".wrap_target",
            "out pc, 2 side 0b01 [3]",
            "set pindirs, 0b00 side 0b01 [3]",
            "out pc, 2 side 0b10 [3]",
            "start:",
            "set pindirs, 0b11 side 0b01",
            ".wrap",
        )
        .program;
        let usb_tx_installed = pio.install(&usb_tx_program).unwrap();
        defmt::assert!(
            usb_tx_installed.offset() == 0,
            "usb_tx must load at origin 0 for `out pc, 2` symbol jumps"
        );

        // Each WS2812 data bit takes 10 PIO cycles. At 8 MHz this produces
        // the required 800 kHz bit rate.
        let program = pio_proc::pio_asm!(
            ".side_set 1",
            ".wrap_target",
            "bitloop:",
            "out x, 1       side 0 [2]",
            "jmp !x do_zero side 1 [1]",
            "jmp bitloop    side 1 [4]",
            "do_zero:",
            "nop            side 0 [4]",
            ".wrap",
        )
        .program;
        let installed = pio.install(&program).unwrap();

        let divider_256 = clocks.system_clock.freq().to_Hz() as u64 * 256 / WS2812_SM_HZ;
        let divider_integer = (divider_256 / 256) as u16;
        let divider_fraction = (divider_256 % 256) as u8;

        let (mut sm, _, mut tx) = PIOBuilder::from_installed_program(installed)
            .side_set_pin_base(WS2812_PIN)
            .clock_divisor_fixed_point(divider_integer, divider_fraction)
            .out_shift_direction(ShiftDirection::Left)
            .autopull(true)
            .pull_threshold(24)
            .build(sm0);
        sm.set_pindirs([(WS2812_PIN, PinDir::Output)]);
        let _sm = sm.start();

        let (mut usb_tx_sm, _, usb_tx) = PIOBuilder::from_installed_program(usb_tx_installed)
            .side_set_pin_base(USB_DP_PIN)
            .set_pins(USB_DP_PIN, 2)
            .clock_divisor_fixed_point(3, 0)
            .out_shift_direction(ShiftDirection::Left)
            .autopull(true)
            .pull_threshold(8)
            .buffers(Buffers::OnlyTx)
            .build(sm1);
        usb_tx_sm.set_pindirs([(USB_DM_PIN, PinDir::Input), (USB_DP_PIN, PinDir::Input)]);
        let usb_tx_sm = usb_tx_sm.start();

        // RX uses PIO1 so the decoder and edge detector have independent
        // instruction space. PIO inputs can observe GPIO12/13 even though the
        // pins' output function is PIO0.
        let (mut rx_pio, rx_sm0, eop_sm1, _, _) = pac.PIO1.split(&mut pac.RESETS);
        // Exact port of Pico-PIO-USB's usb_nrzi_decoder. The edge detector's
        // trigger cadence and this program's per-path cycle counts are tuned
        // together; a restructured variant decodes our own (phase-locked)
        // loopback but loses real devices whose bit phase is independent.
        // x tracks the previous inverted line level, OSR is pre-filled with
        // ones, y counts consecutive ones for stuff-bit removal.
        let nrzi_decoder_program = pio_proc::pio_asm!(
            ".wrap_target",
            "set_y:",
            "set y, 6",
            "irq_wait:",
            "wait 1 irq 4",
            "jmp !y flip",
            "jmp pin pin_high",
            "pin_low:",
            "jmp !x one_bit",
            "zero_bit:",
            "in null, 1",
            "flip:",
            "mov x, ~x",
            ".wrap",
            "pin_high:",
            "jmp !x zero_bit",
            "one_bit:",
            "in osr, 1",
            "jmp y-- irq_wait",
        )
        .program;
        let nrzi_decoder_installed = rx_pio.install(&nrzi_decoder_program).unwrap();
        let decoder_start_address = nrzi_decoder_installed.offset();
        let (mut rx_sm, usb_rx, _) = PIOBuilder::from_installed_program(nrzi_decoder_installed)
            .in_pin_base(USB_DP_PIN)
            .jmp_pin(USB_DP_PIN)
            .in_shift_direction(ShiftDirection::Right)
            .autopush(true)
            .push_threshold(8)
            .buffers(Buffers::OnlyRx)
            .build(rx_sm0);
        rx_sm.set_pindirs([(USB_DM_PIN, PinDir::Input), (USB_DP_PIN, PinDir::Input)]);
        rx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::MOV {
                destination: MovDestination::OSR,
                op: MovOperation::Invert,
                source: MovSource::NULL,
            },
            delay: 0,
            side_set: None,
        });
        rx_sm.exec_instruction(Instruction {
            operands: InstructionOperands::SET {
                destination: SetDestination::X,
                data: 0,
            },
            delay: 0,
            side_set: None,
        });

        // Exact port of Pico-PIO-USB's usb_edge_detector: the resync window
        // layout (2 checks before pin_went_low, 4 after), the [1] delays and
        // the trigger-before-capture order all set the decoder's sampling
        // cadence and must not be reshuffled.
        let edge_detector_program = pio_proc::pio_asm!(
            "eop:",
            "irq wait 2",
            "start:",
            "jmp pin start",
            "irq 3 [1]",
            ".wrap_target",
            "pin_still_low:",
            "irq 4 [1]",
            "pin_low:",
            "jmp pin pin_went_high",
            "jmp pin pin_went_high",
            "pin_went_low:",
            "jmp pin pin_went_high",
            "jmp pin pin_went_high",
            "jmp pin pin_went_high",
            "jmp pin pin_went_high",
            ".wrap",
            "pin_still_high:",
            "mov x, isr [1]",
            "jmp x-- eop",
            "pin_went_high:",
            "mov isr, null [1]",
            "irq 4",
            "in pins, 1",
            "jmp pin pin_still_high",
            "jmp pin_went_low",
        )
        .program;
        let edge_detector_installed = rx_pio.install(&edge_detector_program).unwrap();
        let edge_eop_address = edge_detector_installed.offset();
        let (mut eop_sm, _, _) = PIOBuilder::from_installed_program(edge_detector_installed)
            .in_pin_base(USB_DP_PIN)
            .jmp_pin(USB_DM_PIN)
            .in_shift_direction(ShiftDirection::Left)
            .clock_divisor_fixed_point(1, 128)
            .build(eop_sm1);
        eop_sm.set_pindirs([(USB_DM_PIN, PinDir::Input), (USB_DP_PIN, PinDir::Input)]);
        let rx_sm = rx_sm.start();
        let eop_sm = eop_sm.start();

        // The edge state machine begins at `irq wait 2`, so leave IRQ2 set
        // while transmitting. PioUsbHost clears it only after the final TX
        // symbol, opening the response window without receiving our own packet.
        let mut host = PioUsbHost::new(
            pio,
            usb_tx_sm,
            usb_tx,
            rx_pio,
            rx_sm,
            usb_rx,
            eop_sm,
            edge_eop_address,
            decoder_start_address,
        );

        // Dynamic cloning must know the downstream identity before the native
        // Type-C device connects. Wait for one HID device, snapshot every
        // descriptor and only then construct the upstream USB device.
        watchdog.start(hal::fugit::MicrosDurationU32::millis(5_000));
        let clone_profile = loop {
            watchdog.feed();
            usb_dm.set_input_override(InputOverride::Normal);
            usb_dp.set_input_override(InputOverride::Normal);
            let dm_high = usb_dm.as_input().is_high().unwrap_or(false);
            let dp_high = usb_dp.as_input().is_high().unwrap_or(false);
            usb_dm.set_input_override(InputOverride::Invert);
            usb_dp.set_input_override(InputOverride::Invert);
            let speed = match (dm_high, dp_high) {
                (false, true) => Some(BusSpeed::Full),
                (true, false) => Some(BusSpeed::Low),
                _ => None,
            };
            if let Some(speed) = speed {
                host.configure_speed(speed);
                host.reset_bus(&mut timer);
                match enumerate_clone_device(&mut host, &timer) {
                    Ok(profile) => break profile,
                    Err(error) => warn!(
                        "clone enumeration failed at stage {}: {:?}",
                        error.stage,
                        defmt::Debug2Format(&error.source)
                    ),
                }
            }
            let retry_at = timer.get_counter_low().wrapping_add(250_000);
            while retry_at.wrapping_sub(timer.get_counter_low()) as i32 > 0 {
                watchdog.feed();
            }
        };

        let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
            pac.USB,
            pac.USB_DPRAM,
            clocks.usb_clock,
            true,
            &mut pac.RESETS,
        ));
        let identity = &clone_profile.identity;
        let mut clone =
            dynamic_clone::DynamicHidClone::new(&usb_bus, &mut host, &timer, &clone_profile);
        let mut strings = StringDescriptors::default();
        if let Some(value) = identity.manufacturer.as_str() {
            strings = strings.manufacturer(value);
        }
        if let Some(value) = identity.product.as_str() {
            strings = strings.product(value);
        }
        if let Some(value) = identity.serial.as_str() {
            strings = strings.serial_number(value);
        }
        let mut builder =
            UsbDeviceBuilder::new(&usb_bus, UsbVidPid(identity.vendor_id, identity.product_id))
                .device_class(identity.device_class)
                .device_sub_class(identity.device_subclass)
                .device_protocol(identity.device_protocol)
                .device_release(identity.device_bcd)
                .usb_rev(if identity.usb_bcd >= 0x0210 {
                    UsbRev::Usb210
                } else {
                    UsbRev::Usb200
                })
                .self_powered(clone_profile.attributes & 0x40 != 0)
                .supports_remote_wakeup(clone_profile.attributes & 0x20 != 0)
                .max_packet_size_0(identity.ep0_size)
                .unwrap()
                .max_power(usize::from(clone_profile.max_power_2ma) * 2)
                .unwrap();
        if identity.manufacturer.as_str().is_some()
            || identity.product.as_str().is_some()
            || identity.serial.as_str().is_some()
        {
            builder = builder.strings(&[strings]).unwrap();
        }
        let mut usb_device = builder.build();

        const COLORS: [u32; 4] = [0x00_04_00, 0x04_00_00, 0x00_00_04, 0x00_00_00];
        let mut frame = 0u16;
        let mut color_index = 0usize;
        let mut next_frame_tick = timer.get_counter_low();
        loop {
            watchdog.feed();
            usb_device.poll(&mut [&mut clone]);
            clone.tick(frame);
            if clone.disconnected() {
                hal::reboot::reboot(
                    hal::reboot::RebootKind::Normal,
                    hal::reboot::RebootArch::Arm,
                );
            }
            frame = (frame + 1) & 0x07ff;
            if frame.is_multiple_of(500) {
                while !tx.write(COLORS[color_index] << 8) {}
                color_index = (color_index + 1) % COLORS.len();
            }
            let now = timer.get_counter_low();
            let scheduled = next_frame_tick.wrapping_add(1000);
            next_frame_tick = if scheduled.wrapping_sub(now) as i32 > 0 {
                scheduled
            } else {
                now.wrapping_add(1000)
            };
            while next_frame_tick.wrapping_sub(timer.get_counter_low()) as i32 > 0 {
                usb_device.poll(&mut [&mut clone]);
            }
        }
    }
}

#[cfg(rp2350)]
#[derive(Clone, Copy, Debug)]
struct EnumerationError {
    stage: u8,
    source: TransactionError,
}

#[cfg(rp2350)]
fn enumerate_clone_device<D: hal::timer::TimerDevice>(
    host: &mut PioUsbHost,
    timer: &hal::Timer<D>,
) -> Result<CloneProfile, EnumerationError> {
    let mut device_head = [0u8; 8];
    if host
        .control_read(
            timer,
            0,
            SetupPacket {
                request_type: 0x80,
                request: 6,
                value: 0x0100,
                index: 0,
                length: 8,
            },
            8,
            &mut device_head,
        )
        .map_err(|source| EnumerationError { stage: 1, source })?
        != 8
        || !matches!(device_head[7], 8 | 16 | 32 | 64)
    {
        return Err(EnumerationError {
            stage: 1,
            source: TransactionError::Malformed,
        });
    }
    let ep0_size = usize::from(device_head[7]);
    host.control_write(
        timer,
        0,
        SetupPacket {
            request_type: 0,
            request: 5,
            value: 1,
            index: 0,
            length: 0,
        },
    )
    .map_err(|source| EnumerationError { stage: 2, source })?;
    let address_set_at = timer.get_counter_low();
    while timer.get_counter_low().wrapping_sub(address_set_at) < 2_000 {}

    let mut device_descriptor = [0u8; 18];
    if host
        .control_read(
            timer,
            1,
            SetupPacket {
                request_type: 0x80,
                request: 6,
                value: 0x0100,
                index: 0,
                length: 18,
            },
            ep0_size,
            &mut device_descriptor,
        )
        .map_err(|source| EnumerationError { stage: 3, source })?
        != 18
    {
        return Err(EnumerationError {
            stage: 3,
            source: TransactionError::Malformed,
        });
    }
    let identity = CloneIdentity::parse(&device_descriptor).map_err(|_| EnumerationError {
        stage: 3,
        source: TransactionError::Malformed,
    })?;

    let mut config_head = [0u8; 9];
    if host
        .control_read(
            timer,
            1,
            SetupPacket {
                request_type: 0x80,
                request: 6,
                value: 0x0200,
                index: 0,
                length: 9,
            },
            ep0_size,
            &mut config_head,
        )
        .map_err(|source| EnumerationError { stage: 4, source })?
        != 9
    {
        return Err(EnumerationError {
            stage: 4,
            source: TransactionError::Malformed,
        });
    }
    let total = usize::from(u16::from_le_bytes([config_head[2], config_head[3]]));
    if !(9..=256).contains(&total) {
        return Err(EnumerationError {
            stage: 4,
            source: TransactionError::BufferTooSmall,
        });
    }
    let mut config = [0u8; 256];
    let actual = host
        .control_read(
            timer,
            1,
            SetupPacket {
                request_type: 0x80,
                request: 6,
                value: 0x0200,
                index: 0,
                length: total as u16,
            },
            ep0_size,
            &mut config[..total],
        )
        .map_err(|source| EnumerationError { stage: 4, source })?;
    let mut profile =
        parse_clone_configuration(identity, &config[..actual]).map_err(|_| EnumerationError {
            stage: 4,
            source: TransactionError::Malformed,
        })?;

    host.control_write(
        timer,
        1,
        SetupPacket {
            request_type: 0,
            request: 9,
            value: u16::from(profile.configuration_value),
            index: 0,
            length: 0,
        },
    )
    .map_err(|source| EnumerationError { stage: 5, source })?;

    for interface in profile.interfaces_mut() {
        if interface.report_descriptor_len == 0
            || interface.report_descriptor_len > interface.report_descriptor.len()
        {
            return Err(EnumerationError {
                stage: 6,
                source: TransactionError::BufferTooSmall,
            });
        }
        let wanted = interface.report_descriptor_len;
        let actual = host
            .control_read(
                timer,
                1,
                SetupPacket {
                    request_type: 0x81,
                    request: 6,
                    value: 0x2200,
                    index: u16::from(interface.original_number),
                    length: wanted as u16,
                },
                ep0_size,
                &mut interface.report_descriptor[..wanted],
            )
            .map_err(|source| EnumerationError { stage: 6, source })?;
        interface.report_descriptor_len = actual;
    }

    let language = read_string_language(host, timer, ep0_size).unwrap_or(0x0409);
    profile.identity.manufacturer = read_clone_string(
        host,
        timer,
        ep0_size,
        language,
        profile.identity.manufacturer_index,
    );
    profile.identity.product = read_clone_string(
        host,
        timer,
        ep0_size,
        language,
        profile.identity.product_index,
    );
    profile.identity.serial = read_clone_string(
        host,
        timer,
        ep0_size,
        language,
        profile.identity.serial_index,
    );
    Ok(profile)
}

#[cfg(rp2350)]
fn read_string_language<D: hal::timer::TimerDevice>(
    host: &mut PioUsbHost,
    timer: &hal::Timer<D>,
    ep0_size: usize,
) -> Option<u16> {
    let mut descriptor = [0u8; 4];
    let actual = host
        .control_read(
            timer,
            1,
            SetupPacket {
                request_type: 0x80,
                request: 6,
                value: 0x0300,
                index: 0,
                length: 4,
            },
            ep0_size,
            &mut descriptor,
        )
        .ok()?;
    (actual >= 4).then(|| u16::from_le_bytes([descriptor[2], descriptor[3]]))
}

#[cfg(rp2350)]
fn read_clone_string<D: hal::timer::TimerDevice>(
    host: &mut PioUsbHost,
    timer: &hal::Timer<D>,
    ep0_size: usize,
    language: u16,
    index: u8,
) -> CloneString {
    if index == 0 {
        return CloneString::empty();
    }
    let mut descriptor = [0u8; 128];
    let Ok(actual) = host.control_read(
        timer,
        1,
        SetupPacket {
            request_type: 0x80,
            request: 6,
            value: 0x0300 | u16::from(index),
            index: language,
            length: descriptor.len() as u16,
        },
        ep0_size,
        &mut descriptor,
    ) else {
        return CloneString::empty();
    };
    CloneString::from_usb_descriptor(&descriptor[..actual])
}

/// Program metadata for `picotool info`
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [hal::binary_info::EntryAddr; 5] = [
    hal::binary_info::rp_cargo_bin_name!(),
    hal::binary_info::rp_cargo_version!(),
    hal::binary_info::rp_program_description!(c"Blinky Example"),
    hal::binary_info::rp_cargo_homepage_url!(),
    hal::binary_info::rp_program_build_attribute!(),
];

// End of file

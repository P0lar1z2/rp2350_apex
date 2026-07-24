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
mod debug_hid;
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
use debug_hid::{
    DebugHid, DebugState, EVENT_BOOT, EVENT_BUS_RESET, EVENT_DECODER_TEST, EVENT_ENUM_FAILED,
    EVENT_ENUM_OK, EVENT_ENUM_START, EVENT_HID_DECODED, EVENT_HID_REPORT, EVENT_LINE_STATE,
    EVENT_PIO_LOOPBACK, EVENT_TRANSACTION_ERROR, EVENT_TX_STATE,
};
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
use pio_host::{BusSpeed, InResult, PioUsbHost, TransactionError};
#[cfg(rp2350)]
use t2::hid_device::KeyboardMouseHid;
#[cfg(rp2350)]
use t2::usb_host::{
    DecodedReport, HidConfiguration, HidEndpoint, HidKind, SetupPacket, UsbPid,
    parse_report_descriptor,
};
#[cfg(rp2350)]
use usb_device::{
    bus::UsbBusAllocator,
    device::{StringDescriptors, UsbDeviceBuilder, UsbVidPid},
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

        // Native Type-C port: a two-interface boot keyboard + boot mouse USB
        // device, implemented directly on the Rust `usb-device` traits.
        let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
            pac.USB,
            pac.USB_DPRAM,
            clocks.usb_clock,
            true,
            &mut pac.RESETS,
        ));
        let mut hid = KeyboardMouseHid::new(&usb_bus);
        let mut debug_hid = DebugHid::new(&usb_bus);
        let mut usb_device = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x1209, 0x2350))
            .strings(&[StringDescriptors::default()
                .manufacturer("Pure Rust RP2350")
                .product("PIO Host Keyboard + Mouse Bridge")
                .serial_number("RP2350-USB-A")])
            .unwrap()
            .device_class(0)
            .build();

        // WS2812 data order is GRB. Keep the values low so the tiny LED is
        // comfortable to look at: red, green, blue, then off.
        const COLORS: [u32; 4] = [0x00_04_00, 0x04_00_00, 0x00_00_04, 0x00_00_00];
        let mut frame = 0u16;
        let mut color_index = 0usize;
        let mut endpoints = [None; 4];
        let mut expected_pid = [UsbPid::Data0; 4];
        let mut next_poll = [0u16; 4];
        let mut endpoint_error_streak = [0u8; 4];
        let mut endpoint_count = 0usize;
        let mut retry_enumeration_at = 0u16;
        let mut next_frame_tick = timer.get_counter_low();
        let mut detected_speed = 0u8;
        let mut debug_state = DebugState::new();
        // Distinguishes a watchdog recovery (bit0 = timer) from a cold boot in
        // the BOOT telemetry event.
        let reboot_reason = unsafe { (*hal::pac::WATCHDOG::ptr()).reason().read().bits() as u8 };
        debug_state.record(
            EVENT_BOOT,
            timer.get_counter_low(),
            0,
            0,
            0,
            0,
            &[reboot_reason],
        );
        // The PIO busy-waits are individually bounded, but a wedged state
        // machine or an unforeseen stall must never leave the board looking
        // dead on both USB ports again. Worst-case legal loop iteration is a
        // full enumeration retry (~1 s of NAK-bounded control transfers).
        watchdog.start(hal::fugit::MicrosDurationU32::millis(5_000));
        let mut sof_failed = false;
        loop {
            watchdog.feed();
            usb_device.poll(&mut [&mut hid, &mut debug_hid]);
            debug_state.try_send(&debug_hid);
            match debug_hid.take_reboot_request() {
                1 => hal::reboot::reboot(
                    hal::reboot::RebootKind::Normal,
                    hal::reboot::RebootArch::Arm,
                ),
                2 => hal::reboot::reboot(
                    hal::reboot::RebootKind::BootSel {
                        picoboot_disabled: false,
                        msd_disabled: false,
                    },
                    hal::reboot::RebootArch::Arm,
                ),
                _ => {}
            }

            if endpoint_count == 0 && frame == retry_enumeration_at {
                // A full-speed device pulls D+ high; a low-speed device pulls
                // D- high. Temporarily remove the RX inversion to inspect the
                // physical line state, then restore it for the PIO programs.
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
                detected_speed = match speed {
                    Some(BusSpeed::Full) => 1,
                    Some(BusSpeed::Low) => 2,
                    None => 0,
                };
                let pin_configuration = host.pio_pin_configuration();
                debug_state.record(
                    EVENT_LINE_STATE,
                    timer.get_counter_low(),
                    detected_speed,
                    0,
                    0,
                    0,
                    &[
                        dm_high as u8,
                        dp_high as u8,
                        usb_dm.get_input_override() as u8,
                        usb_dp.get_input_override() as u8,
                        pin_configuration[0],
                        pin_configuration[1],
                        pin_configuration[2],
                    ],
                );

                let result = if let Some(speed) = speed {
                    debug_state.record(
                        EVENT_ENUM_START,
                        timer.get_counter_low(),
                        detected_speed,
                        1,
                        0,
                        0,
                        &[],
                    );
                    host.configure_speed(speed);
                    host.clear_rx_diagnostic();
                    let decoder_test = host.decoder_test();
                    let mut diagnostic = [0u8; 40];
                    let diagnostic_length = host.rx_diagnostic(&mut diagnostic);
                    debug_state.record(
                        EVENT_DECODER_TEST,
                        timer.get_counter_low(),
                        detected_speed,
                        0,
                        decoder_test.err().map_or(0, transaction_error_code),
                        0,
                        &diagnostic[..diagnostic_length],
                    );
                    host.clear_rx_diagnostic();
                    let loopback = host.loopback_test(&timer);
                    let diagnostic_length = host.rx_diagnostic(&mut diagnostic);
                    debug_state.record(
                        EVENT_PIO_LOOPBACK,
                        timer.get_counter_low(),
                        detected_speed,
                        0,
                        loopback.err().map_or(0, transaction_error_code),
                        0,
                        &diagnostic[..diagnostic_length],
                    );
                    debug_state.record(
                        EVENT_TX_STATE,
                        timer.get_counter_low(),
                        detected_speed,
                        0,
                        0,
                        0,
                        &host.tx_diagnostic(),
                    );
                    host.clear_rx_diagnostic();
                    host.reset_bus(&mut timer);
                    debug_state.record(
                        EVENT_BUS_RESET,
                        timer.get_counter_low(),
                        detected_speed,
                        0,
                        0,
                        0,
                        &host.reset_diagnostic(),
                    );
                    enumerate_hid(&mut host, &timer)
                } else {
                    Err(EnumerationError {
                        stage: 0,
                        source: TransactionError::Timeout,
                    })
                };
                match result {
                    Ok(configuration) => {
                        endpoint_count = 0;
                        for endpoint in configuration.endpoints() {
                            if endpoint_count < endpoints.len() {
                                endpoints[endpoint_count] = Some(endpoint);
                                expected_pid[endpoint_count] = UsbPid::Data0;
                                next_poll[endpoint_count] = frame;
                                endpoint_error_streak[endpoint_count] = 0;
                                endpoint_count += 1;
                            }
                        }
                        let mut endpoint_summary = [0u8; 24];
                        for (index, endpoint) in
                            endpoints[..endpoint_count].iter().flatten().enumerate()
                        {
                            let start = index * 6;
                            endpoint_summary[start] = endpoint.endpoint;
                            endpoint_summary[start + 1] = endpoint.interface;
                            endpoint_summary[start + 2] = match endpoint.kind {
                                HidKind::Keyboard => 1,
                                HidKind::Mouse => 2,
                                HidKind::Generic => 3,
                            };
                            endpoint_summary[start + 3] = !endpoint.decoder.is_empty() as u8;
                            endpoint_summary[start + 4..start + 6]
                                .copy_from_slice(&endpoint.report_descriptor_len.to_le_bytes());
                        }
                        debug_state.record(
                            EVENT_ENUM_OK,
                            timer.get_counter_low(),
                            detected_speed,
                            7,
                            0,
                            endpoint_count as u8,
                            &endpoint_summary[..endpoint_count * 6],
                        );
                        info!("USB-A HID configured: {} endpoints", endpoint_count);
                    }
                    Err(error) => {
                        let mut diagnostic = [0u8; 40];
                        let diagnostic_length = host.rx_diagnostic(&mut diagnostic);
                        debug_state.record(
                            EVENT_ENUM_FAILED,
                            timer.get_counter_low(),
                            detected_speed,
                            error.stage,
                            transaction_error_code(error.source),
                            0,
                            &diagnostic[..diagnostic_length],
                        );
                        // The TX snapshot shows whether the last transmitted
                        // packet made it out completely (queued vs FIFO left).
                        debug_state.record(
                            EVENT_TX_STATE,
                            timer.get_counter_low(),
                            detected_speed,
                            error.stage,
                            transaction_error_code(error.source),
                            0,
                            &host.tx_diagnostic(),
                        );
                        debug!("USB-A enumeration retry: {:?}", defmt::Debug2Format(&error));
                        retry_enumeration_at = frame.wrapping_add(1000) & 0x07ff;
                    }
                }
            }

            // A stuck transmitter would previously hang here forever; now it
            // reports once per failure streak with the TX state machine's
            // PC/IRQ/FIFO snapshot.
            match host.send_sof(frame) {
                Ok(()) => sof_failed = false,
                Err(_) if !sof_failed => {
                    sof_failed = true;
                    debug_state.record(
                        EVENT_TX_STATE,
                        timer.get_counter_low(),
                        detected_speed,
                        0,
                        transaction_error_code(TransactionError::Timeout),
                        0,
                        &host.tx_diagnostic(),
                    );
                }
                Err(_) => {}
            }

            let active_endpoint_count = endpoint_count;
            for index in 0..active_endpoint_count {
                let Some(endpoint) = endpoints[index] else {
                    continue;
                };
                if frame != next_poll[index] {
                    continue;
                }
                next_poll[index] = frame.wrapping_add(u16::from(endpoint.interval_ms)) & 0x07ff;
                let mut report = [0u8; 64];
                match host.input(&timer, 1, endpoint.endpoint, &mut report) {
                    Ok(InResult::Nak) => endpoint_error_streak[index] = 0,
                    Ok(InResult::Data { length, pid }) if pid == expected_pid[index] => {
                        endpoint_error_streak[index] = 0;
                        expected_pid[index] = if pid == UsbPid::Data0 {
                            UsbPid::Data1
                        } else {
                            UsbPid::Data0
                        };
                        if let Some(decoded) = forward_hid_report(&hid, endpoint, &report[..length])
                        {
                            let (payload, payload_len) =
                                encode_decoded_debug(decoded, &report[..length]);
                            debug_state.record(
                                EVENT_HID_DECODED,
                                timer.get_counter_low(),
                                detected_speed,
                                0,
                                0,
                                endpoint.endpoint,
                                &payload[..payload_len],
                            );
                        } else {
                            debug_state.record(
                                EVENT_HID_REPORT,
                                timer.get_counter_low(),
                                detected_speed,
                                0,
                                0,
                                endpoint.endpoint,
                                &report[..length],
                            );
                        }
                    }
                    Ok(InResult::Data { .. }) => endpoint_error_streak[index] = 0,
                    Err(TransactionError::Stall) => {
                        debug_state.record(
                            EVENT_TRANSACTION_ERROR,
                            timer.get_counter_low(),
                            detected_speed,
                            0,
                            transaction_error_code(TransactionError::Stall),
                            endpoint.endpoint,
                            &[],
                        );
                        endpoint_count = 0;
                        retry_enumeration_at = frame.wrapping_add(1000) & 0x07ff;
                        break;
                    }
                    Err(error) => {
                        endpoint_error_streak[index] =
                            endpoint_error_streak[index].saturating_add(1);
                        // One transient failure is useful telemetry; emitting
                        // every failed poll floods the debug endpoint and
                        // hides the event that caused the streak. A real USB
                        // device answers an interrupt IN with DATA or NAK, so
                        // repeated silence means it was unplugged or replaced
                        // and our address/endpoint state is stale.
                        if endpoint_error_streak[index] == 1 {
                            debug_state.record(
                                EVENT_TRANSACTION_ERROR,
                                timer.get_counter_low(),
                                detected_speed,
                                0,
                                transaction_error_code(error),
                                endpoint.endpoint,
                                &[],
                            );
                        }
                        if endpoint_error_streak[index] >= 8 {
                            endpoint_count = 0;
                            retry_enumeration_at = frame.wrapping_add(1) & 0x07ff;
                            break;
                        }
                    }
                }
                usb_device.poll(&mut [&mut hid, &mut debug_hid]);
                debug_state.try_send(&debug_hid);
            }

            frame = (frame + 1) & 0x07ff;
            if frame.is_multiple_of(500) {
                while !tx.write(COLORS[color_index] << 8) {}
                color_index = (color_index + 1) % COLORS.len();
            }
            // Keep SOF cadence tied to the 1 MHz hardware timer. Poll the
            // native Type-C device while waiting, so PC control requests and
            // HID IN transfers remain responsive.
            let now = timer.get_counter_low();
            let scheduled = next_frame_tick.wrapping_add(1000);
            next_frame_tick = if scheduled.wrapping_sub(now) as i32 > 0 {
                scheduled
            } else {
                now.wrapping_add(1000)
            };
            while next_frame_tick.wrapping_sub(timer.get_counter_low()) as i32 > 0 {
                usb_device.poll(&mut [&mut hid, &mut debug_hid]);
                debug_state.try_send(&debug_hid);
            }
        }
    }
}

#[cfg(rp2350)]
const fn transaction_error_code(error: TransactionError) -> u8 {
    match error {
        TransactionError::Timeout => 1,
        TransactionError::Malformed => 2,
        TransactionError::Stall => 3,
        TransactionError::BufferTooSmall => 4,
    }
}

#[cfg(rp2350)]
#[derive(Clone, Copy, Debug)]
struct EnumerationError {
    stage: u8,
    source: TransactionError,
}

#[cfg(rp2350)]
fn enumerate_hid<D: hal::timer::TimerDevice>(
    host: &mut PioUsbHost,
    timer: &hal::Timer<D>,
) -> Result<HidConfiguration, EnumerationError> {
    // First eight bytes reveal endpoint-zero's maximum packet size.
    let mut device_head = [0u8; 8];
    let length = host
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
        .map_err(|source| EnumerationError { stage: 1, source })?;
    if length != 8 || !matches!(device_head[7], 8 | 16 | 32 | 64) {
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
            request: 5, // SET_ADDRESS
            value: 1,
            index: 0,
            length: 0,
        },
    )
    .map_err(|source| EnumerationError { stage: 2, source })?;

    // USB 2.0 requires a recovery interval after SET_ADDRESS before the host
    // uses the new address. Wireless receivers often need nearly the full
    // allowance while their internal hub/HID MCU updates endpoint zero.
    let address_set_at = timer.get_counter_low();
    while timer.get_counter_low().wrapping_sub(address_set_at) < 2_000 {}

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
        .map_err(|source| EnumerationError { stage: 3, source })?
        != 9
    {
        return Err(EnumerationError {
            stage: 3,
            source: TransactionError::Malformed,
        });
    }
    let total = usize::from(u16::from_le_bytes([config_head[2], config_head[3]]));
    if !(9..=256).contains(&total) {
        return Err(EnumerationError {
            stage: 3,
            source: TransactionError::BufferTooSmall,
        });
    }
    let mut config_bytes = [0u8; 256];
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
            &mut config_bytes[..total],
        )
        .map_err(|source| EnumerationError { stage: 4, source })?;
    let mut configuration = t2::usb_host::parse_hid_configuration(&config_bytes[..actual])
        .map_err(|_| EnumerationError {
            stage: 4,
            source: TransactionError::Malformed,
        })?;
    if configuration.is_empty() {
        return Err(EnumerationError {
            stage: 4,
            source: TransactionError::Malformed,
        });
    }

    host.control_write(
        timer,
        1,
        SetupPacket {
            request_type: 0,
            request: 9, // SET_CONFIGURATION
            value: u16::from(configuration.configuration_value),
            index: 0,
            length: 0,
        },
    )
    .map_err(|source| EnumerationError { stage: 5, source })?;

    // Every HID interface owns its Report Descriptor and decoder. This state
    // lives in the endpoint record and is discarded on disconnect/re-enumeration.
    for endpoint in configuration.endpoints_mut() {
        let report_len = usize::from(endpoint.report_descriptor_len);
        if report_len == 0 || report_len > 256 {
            continue;
        }
        let mut report_descriptor = [0u8; 256];
        if let Ok(actual) = host.control_read(
            timer,
            1,
            SetupPacket {
                request_type: 0x81,
                request: 6,
                value: 0x2200,
                index: u16::from(endpoint.interface),
                length: report_len as u16,
            },
            ep0_size,
            &mut report_descriptor[..report_len],
        ) {
            endpoint.decoder = parse_report_descriptor(&report_descriptor[..actual]);
        }
    }

    // Boot-capable interfaces support SET_PROTOCOL. Prefer Report Protocol
    // when its descriptor parsed successfully; retain Boot Protocol only as a
    // compatibility fallback for malformed legacy devices.
    for endpoint in configuration.endpoints() {
        if matches!(endpoint.kind, HidKind::Keyboard | HidKind::Mouse) {
            let _ = host.control_write(
                timer,
                1,
                SetupPacket {
                    request_type: 0x21,
                    request: 0x0b, // HID SET_PROTOCOL
                    value: u16::from(!endpoint.decoder.is_empty()),
                    index: u16::from(endpoint.interface),
                    length: 0,
                },
            );
        }
    }
    Ok(configuration)
}

#[cfg(rp2350)]
fn forward_hid_report<B: usb_device::bus::UsbBus>(
    hid: &KeyboardMouseHid<'_, B>,
    endpoint: HidEndpoint,
    report: &[u8],
) -> Option<DecodedReport> {
    if let Some(decoded) = endpoint.decoder.decode(report) {
        match decoded {
            DecodedReport::Keyboard(state) => {
                let _ = hid.push_keyboard_state(&state);
            }
            DecodedReport::Mouse(state) => {
                let _ = hid.push_mouse_extended(
                    state.buttons,
                    state.x,
                    state.y,
                    state.wheel,
                    state.pan,
                );
            }
            DecodedReport::Consumer(usage) => {
                let _ = hid.push_consumer(usage);
            }
        }
        return Some(decoded);
    }

    // If an old boot device supplied no usable descriptor, preserve the
    // fixed-format path instead of dropping its input entirely.
    match endpoint.kind {
        HidKind::Keyboard if report.len() >= 8 => {
            let mut boot = [0u8; 8];
            boot.copy_from_slice(&report[..8]);
            let _ = hid.push_keyboard(&boot);
        }
        HidKind::Mouse if report.len() >= 3 => {
            let wheel = report.get(3).copied().unwrap_or(0) as i8;
            let _ = hid.push_mouse(report[0], report[1] as i8, report[2] as i8, wheel);
        }
        _ => {}
    }
    None
}

#[cfg(rp2350)]
fn encode_decoded_debug(decoded: DecodedReport, raw: &[u8]) -> ([u8; 40], usize) {
    let mut payload = [0u8; 40];
    let normalized_len = match decoded {
        DecodedReport::Keyboard(state) => {
            payload[0] = 1;
            payload[1] = state.modifiers;
            payload[2..16].copy_from_slice(&state.keys);
            16
        }
        DecodedReport::Mouse(state) => {
            payload[0] = 2;
            payload[1] = state.buttons;
            payload[2..4].copy_from_slice(&state.x.to_le_bytes());
            payload[4..6].copy_from_slice(&state.y.to_le_bytes());
            payload[6] = state.wheel as u8;
            payload[7] = state.pan as u8;
            8
        }
        DecodedReport::Consumer(usage) => {
            payload[0] = 3;
            payload[1..3].copy_from_slice(&usage.to_le_bytes());
            3
        }
    };
    let raw_len = raw.len().min(payload.len() - normalized_len - 1);
    payload[normalized_len] = raw_len as u8;
    payload[normalized_len + 1..normalized_len + 1 + raw_len].copy_from_slice(&raw[..raw_len]);
    (payload, normalized_len + 1 + raw_len)
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

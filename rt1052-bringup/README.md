# RT1052 Pro Rust bring-up

RAM-only Rust bring-up for the EmbedFire i.MX RT1052 Pro board with a
`MIMXRT1052CVL5B`. The external W25Q256 FlexSPI flash is not erased or
programmed.

## Programs

- `rt1052-bringup`: the original LED smoke test.
- `host_probe`: verifies the NXP default 128 KiB ITCM + 128 KiB DTCM +
  256 KiB OCRAM split by testing a 32-byte DMA window.
- `host_enumerate`: NXP USB Host 2.12.2 bare-metal EHCI + Hub probe for OTG2.
  It reports HID VID/PID, speed, hub path, Interrupt IN address,
  `wMaxPacketSize`, and `bInterval` over RTT.

## Build

```sh
cargo build --bin host_probe
cargo build --features nxp-host --bin host_enumerate
```

The NXP build needs the locally ignored `.vendor/` tree documented in
[`NXP_SDK.md`](NXP_SDK.md). GNU Arm Embedded builds the C objects; Rust links
them into the same RAM ELF. Keil and FreeRTOS are not involved.

## RAM-only launch

`probe-rs run` performs a post-download hardware reset on this target, which
returns to the external FlexSPI image. Build with Cargo, then use the RAM runner
to set the ELF entry without another reset:

```sh
cargo build --bin host_probe
./tools/ram-run.py target/thumbv7em-none-eabihf/debug/host_probe

cargo build --features nxp-host --bin host_enumerate
./tools/ram-run.py --seconds 15 \
  target/thumbv7em-none-eabihf/debug/host_enumerate
```

The small runner attaches and resets directly into a halt, preserving NXP's
default 128 KiB ITCM + 128 KiB DTCM + 256 KiB OCRAM partition. It then writes
ELF `PT_LOAD` contents and core registers and never performs a post-download
reset. The target runs continuously and is paused only once at the end to
collect RTT and diagnostic state, which is friendlier to the five-wire fireDAP
link. If the five-pin NRST signal is known to work, `--connect-under-reset` can
be added. The ELF is downloaded only into internal RAM; external FlexSPI flash
is not erased or programmed.

With the five-wire fireDAP connection, a lost debug session may require pressing
the board's physical `RESET/RST` button or power-cycling before the next launch.

The intended wiring is OTG2 to the board's FE1.1S hub and mouse, while OTG1 is
connected to the PC. `host_enumerate` only exercises OTG2.

## Verified hardware result

The connected `17ef:62c2` composite receiver enumerates its mouse as interface
1, protocol 2, through hub address 1 port 3. Its Interrupt IN endpoint is
`0x82`, with an 8-byte maximum packet and `bInterval=1`. It is a Full-Speed
source, so this path has a 1 ms / 1 kHz ceiling; it must not be advertised as
8 kHz. Continuous receive has been verified with 7-byte raw mouse reports.

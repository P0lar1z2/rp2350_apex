# RT1052 Pro Rust bring-up

Rust bring-up for the EmbedFire i.MX RT1052 Pro board with a
`MIMXRT1052CVL5B`. Development remains RAM-first; a guarded W25Q256 backup,
boot-image, programming, and readback-verification flow is also available.

## Programs

- `rt1052-bringup`: the original LED smoke test.
- `host_probe`: verifies the NXP default 128 KiB ITCM + 128 KiB DTCM +
  256 KiB OCRAM split by testing a 32-byte DMA window.
- `host_enumerate`: NXP USB Host 2.12.2 bare-metal EHCI + Hub probe for OTG2.
  It reports HID VID/PID, speed, hub path, Interrupt IN address,
  `wMaxPacketSize`, and `bInterval` over RTT.
- `enet_probe`: reads the Pro board's LAN8720A identity and link state over MDIO.
- `enet_dma_probe`: initializes five RX and three TX descriptors in non-cacheable
  OCRAM, then polls raw frames without an RTOS.
- `gamepad_bridge`: converts an OTG2 keyboard and mouse into one fixed OTG1
  Microsoft XInputHID-compatible gamepad. See
  [`GAMEPAD_CONVERTER.md`](../GAMEPAD_CONVERTER.md) for the Apex-oriented
  mapping, limits, and hardware acceptance plan.

## Build

```sh
cargo build --bin host_probe
cargo build --features nxp-host --bin host_enumerate
cargo build --features nxp-enet --bin enet_dma_probe
```

From the repository root, build the gamepad converter with the workspace-level
RT1052 linker configuration (avoiding duplicate child/parent `-Tlink.x` flags):

```sh
cargo build --manifest-path rt1052-bringup/Cargo.toml \
  --target thumbv7em-none-eabihf \
  --features nxp-host,nxp-device --bin gamepad_bridge
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

## FlexSPI Flash

The Flash workflow uses pyOCD's `mimxrt1050_quadspi` target. It preserves the
known-good 8 KiB board-specific FCB/IVT/DCD header already on the board, links
the Rust application at `0x60002000`, and verifies the programmed bytes by
reading them back. A `flash` operation always saves and validates the complete
32 MiB W25Q256 first; `.flash/` is ignored by Git.

Install the pinned programmer once into the ignored local environment:

```sh
python3 -m venv .venv-pyocd
.venv-pyocd/bin/pip install pyocd==0.45.1
```

```sh
# Read-only backup. Global options must precede the subcommand.
./tools/flash.py backup

# Offline image build from a validated backup.
./tools/flash.py build .flash/w25q256-YYYYMMDD-HHMMSS.bin

# Destructive step: backup again, erase only occupied sectors, program, verify.
./tools/flash.py flash --yes

# Build/flash the fixed HID gamepad converter instead of the HID clone.
# Global options must appear before the subcommand.
./tools/flash.py --bin gamepad_bridge flash --yes

# Reuse a complete validated backup after a transient DAP disconnect.
./tools/flash.py --bin gamepad_bridge flash \
  --backup .flash/w25q256-YYYYMMDD-HHMMSS.bin --yes

# Retry readback/boot only after a transient DAP reconnect failure.
./tools/flash.py verify .flash/hid_bridge-rt1052-boot.bin
```

The generated image is `.flash/<binary>-rt1052-boot.bin`. Without `--yes`, the
`flash` command stops after backup and image creation. Keep at least one full
backup outside the repository before programming. The workflow does not burn
eFuses and does not perform a whole-chip erase.

On Windows, verify that the inbox XInput layer sees the converter independently
of a game:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\check-xinput.ps1
```

The script calls the system `xinput1_4.dll` directly and prints only state
changes for slots 0 through 3. `NO_XINPUT_CONTROLLER` means that Windows bound
only its generic HID/DirectInput path or that no XInput controller is present.

Hardware verification programmed 108,576 image bytes after erasing two 64 KiB
sectors. The complete programmed range matched its SHA-256 readback, then an
NRST boot enumerated the five-interface `RT1052 Composite HID Clone` on OTG1.

The intended wiring is OTG2 to the board's FE1.1S hub and mouse, while OTG1 is
connected to the PC. `host_enumerate` only exercises OTG2.

## Verified hardware result

The connected `17ef:62c2` composite receiver enumerates its mouse as interface
1, protocol 2, through hub address 1 port 3. Its Interrupt IN endpoint is
`0x82`, with an 8-byte maximum packet and `bInterval=1`. It is a Full-Speed
source, so this path has a 1 ms / 1 kHz ceiling; it must not be advertised as
8 kHz. Continuous receive has been verified with 7-byte raw mouse reports.

The Pro board PHY responds at MDIO address 0 with ID `0007:c0f1`, identifying a
LAN8720A. ENET DMA initialization and the 5 RX / 3 TX descriptor layout have
been verified in RAM. The control plane uses a small allocation-free `RTCP` v1
datagram format and a bounded queue: network congestion may drop a command or
ACK, but it never blocks the USB data path.

`enet_control` obtains its IPv4 configuration through DHCP and binds UDP port
1052 only after a lease is configured. Its locally administered MAC address is
`02:10:52:00:00:01`; hardware verification obtained a DHCP lease on the
`192.168.110.0/24` network with gateway `192.168.110.1`. The RAM loader must
raise the core/AHB clock to 528 MHz and IPG to 132 MHz before starting ENET;
with the Boot ROM's low-speed clock tree, 10M works but 100M frames are
corrupted. With the RUN clock configured, the PHY-local 100M loopback is
byte-exact and the external link negotiates 100M Full-Duplex.

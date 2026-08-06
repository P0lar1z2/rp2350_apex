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
- `hid_bridge`: clones all downstream HID interfaces from OTG2 to OTG1, keeps
  physical mouse movement unmodified, and replays the 30-shot R-301
  compensation trajectory while the physical left button is held. It obtains
  a DHCP lease and accepts runtime sensitivity changes over UDP port 1052.

## Build

```sh
cargo build --bin host_probe
cargo build --features nxp-host --bin host_enumerate
cargo build --features nxp-enet --bin enet_dma_probe
cargo build --release --features nxp-host,nxp-device,nxp-enet,flash-xip \
  --bin hid_bridge
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

# Retry readback/boot only after a transient DAP reconnect failure.
./tools/flash.py verify .flash/hid_bridge-rt1052-boot.bin
```

The generated image is `.flash/hid_bridge-rt1052-boot.bin`. Without `--yes`,
the `flash` command stops after backup and image creation. Keep at least one
full backup outside the repository before programming. The workflow does not
burn eFuses and does not perform a whole-chip erase.

A previous clone-only build was hardware-verified after programming 108,576
image bytes and checking its SHA-256 readback; NRST then enumerated the
five-interface `RT1052 Composite HID Clone` on OTG1. The combined clone,
trajectory, and ENET image described below still requires hardware validation.

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
datagram format. The bridge validates and applies sensitivity commands from its
main polling loop, outside USB and ENET interrupt paths; network congestion may
drop a command or ACK, but it never blocks HID forwarding.

`enet_control` obtains its IPv4 configuration through DHCP and binds UDP port
1052 only after a lease is configured. Its locally administered MAC address is
`02:10:52:00:00:01`; hardware verification obtained a DHCP lease on the
`192.168.110.0/24` network with gateway `192.168.110.1`. The RAM loader must
raise the core/AHB clock to 528 MHz and IPG to 132 MHz before starting ENET;
with the Boot ROM's low-speed clock tree, 10M works but 100M frames are
corrupted. With the RUN clock configured, the PHY-local 100M loopback is
byte-exact and the external link negotiates 100M Full-Duplex.

## Runtime recoil sensitivity

The bridge starts at game sensitivity `1.000`. The checked-in trajectory was
calibrated at that value; changing sensitivity divides only generated recoil
movement by the requested value. Physical mouse X/Y reports remain 1:1 and the
real left button remains visible to the PC.

After RTT prints the DHCP address, set sensitivity from this repository with
the standard-library-only Python client:

```sh
python rt1052-bringup/tools/rtcp_control.py \
  192.168.110.123 sensitivity 1.5
```

Valid values are `0.100` through `10.000`. The client retries a lost UDP packet
and succeeds only after receiving a matching RTCP ACK. The combined clone,
trajectory, and ENET image exceeds the RT1052's 128 KiB RAM-only ITCM region,
so `hid_bridge` must currently be built with `flash-xip` and installed through
the guarded FlexSPI workflow above.

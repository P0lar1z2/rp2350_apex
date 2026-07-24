"""Linux monitor for the RP2350 bridge's vendor HID debug interface.

Reads /dev/hidrawN directly (no hidapi needed). Finds the vendor debug
interface (bInterfaceNumber 3) of VID 1209 PID 2350 via sysfs.

Run with:  python3 debug_monitor_linux.py [--seconds N] [--bootsel | --reset]
"""

from __future__ import annotations

import argparse
import array
import fcntl
import os
import sys
import time

VID = "1209"
PID = "2350"

EVENTS = {
    1: "BOOT",
    2: "LINE_STATE",
    3: "ENUM_START",
    4: "ENUM_FAILED",
    5: "ENUM_OK",
    6: "HID_REPORT",
    7: "TRANSACTION_ERROR",
    8: "PIO_LOOPBACK",
    9: "DECODER_TEST",
    10: "BUS_RESET",
    11: "TX_STATE",
    12: "HID_DECODED",
}
SPEEDS = {0: "none", 1: "full-speed", 2: "low-speed"}
STAGES = {
    0: "line/detect",
    1: "GET_DEVICE_DESCRIPTOR_8",
    2: "SET_ADDRESS",
    3: "GET_CONFIG_HEADER",
    4: "GET_CONFIG_FULL/PARSE",
    5: "SET_CONFIGURATION",
    6: "SET_PROTOCOL",
    7: "RUNNING",
}
ERRORS = {0: "ok", 1: "timeout", 2: "malformed", 3: "stall", 4: "buffer-too-small"}


def find_debug_hidraw() -> str | None:
    base = "/sys/class/hidraw"
    for name in sorted(os.listdir(base)):
        device = os.path.realpath(os.path.join(base, name, "device"))
        # .../3-10:1.2/0003:1209:2350.000A — interface dir ends in :1.<iface>
        interface_dir = os.path.dirname(device)
        uevent = os.path.join(device, "uevent")
        try:
            text = open(uevent).read()
        except OSError:
            continue
        if f"HID_ID=0003:0000{VID.upper()}:0000{PID.upper()}" not in text.upper():
            continue
        if interface_dir.endswith(":1.3"):
            return f"/dev/{name}"
    return None


def send_feature(fd: int, payload: bytes) -> None:
    # HIDIOCSFEATURE(len) = _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, len)
    report = bytes([0]) + payload + bytes(64 - len(payload))
    buf = array.array("B", report)
    request = 0xC0000000 | (len(buf) << 16) | (ord("H") << 8) | 0x06
    fcntl.ioctl(fd, request, buf, True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=float, default=0, help="Exit after this many seconds")
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--bootsel", action="store_true", help="Reboot the board into BOOTSEL")
    action.add_argument("--reset", action="store_true", help="Perform a normal software reboot")
    args = parser.parse_args()

    path = find_debug_hidraw()
    if path is None:
        print("Debug HID interface not found. Flash the debug firmware and reconnect Type-C.")
        return 1

    fd = os.open(path, os.O_RDWR)
    try:
        if args.bootsel or args.reset:
            command = b"PBOOT" if args.bootsel else b"PRESET"
            send_feature(fd, command)
            print(f"Sent {command.decode()}.", flush=True)
            time.sleep(0.7)
            return 0

        print(f"Connected: {path}", flush=True)
        print("Waiting for telemetry; press Ctrl+C to stop.\n", flush=True)
        deadline = time.monotonic() + args.seconds if args.seconds > 0 else None
        while True:
            if deadline is not None and time.monotonic() >= deadline:
                return 0
            report = os.read(fd, 64)
            if report[:4] != b"PDBG":
                continue
            event, speed, stage = report[5], report[6], report[7]
            sequence = int.from_bytes(report[8:12], "little")
            timestamp = int.from_bytes(report[12:16], "little")
            error, endpoint, length = report[16], report[17], report[18]
            payload = report[20 : 20 + min(length, 40)]
            extra = ""
            if event == 2 and len(payload) >= 2:
                extra = f" D-={payload[0]} D+={payload[1]}"
                if len(payload) >= 7:
                    extra += (
                        f" invert={payload[2]}/{payload[3]}"
                        f" pio(dec-jmp/edge-jmp/edge-in)="
                        f"{payload[4]}/{payload[5]}/{payload[6]}"
                    )
            elif event == 10 and len(payload) >= 7:
                extra = (
                    f" inv-pins(before/asserted/end/released/recovery)="
                    f"{payload[0]:02b}/{payload[1]:02b}/{payload[2]:02b}/"
                    f"{payload[3]:02b}/{payload[4]:02b}"
                    f" tx-pc={payload[5]} tx-irq=0x{payload[6]:02x}"
                )
            elif event == 11 and len(payload) >= 5:
                extra = (
                    f" requested={payload[0]} queued={payload[1]}"
                    f" fifo={payload[2]} irq=0x{payload[3]:02x} pc={payload[4]}"
                )
            elif event == 5 and payload:
                kinds = {1: "keyboard", 2: "mouse", 3: "generic"}
                entries = []
                for offset in range(0, len(payload) - 5, 6):
                    ep, iface, kind, parsed = payload[offset : offset + 4]
                    report_len = int.from_bytes(payload[offset + 4 : offset + 6], "little")
                    entries.append(
                        f"ep{ep}/if{iface}/{kinds.get(kind, kind)}"
                        f"/decoder={'yes' if parsed else 'no'}/desc={report_len}"
                    )
                extra = " " + ", ".join(entries)
            elif event == 12 and payload:
                kind = payload[0]
                if kind == 1 and len(payload) >= 17:
                    raw_len = payload[16]
                    raw = payload[17 : 17 + raw_len]
                    extra = (
                        f" keyboard modifiers=0x{payload[1]:02x}"
                        f" keys={payload[2:16].hex(' ')} raw={raw.hex(' ')}"
                    )
                elif kind == 2 and len(payload) >= 9:
                    raw_len = payload[8]
                    raw = payload[9 : 9 + raw_len]
                    extra = (
                        f" mouse buttons=0x{payload[1]:02x}"
                        f" x={int.from_bytes(payload[2:4], 'little', signed=True)}"
                        f" y={int.from_bytes(payload[4:6], 'little', signed=True)}"
                        f" wheel={int.from_bytes(payload[6:7], 'little', signed=True)}"
                        f" pan={int.from_bytes(payload[7:8], 'little', signed=True)}"
                        f" raw={raw.hex(' ')}"
                    )
                elif kind == 3 and len(payload) >= 4:
                    raw_len = payload[3]
                    raw = payload[4 : 4 + raw_len]
                    extra = (
                        f" consumer usage=0x{int.from_bytes(payload[1:3], 'little'):04x}"
                        f" raw={raw.hex(' ')}"
                    )
            elif event in (4, 8, 9) and len(payload) >= 6:
                kinds = {
                    0: "none", 1: "setup-ack", 2: "in-data", 3: "out-ack",
                    4: "loopback",
                    5: "decoder",
                    0x81: "setup-timeout",
                    0x82: "in-timeout",
                    0x83: "out-timeout",
                    0x84: "loopback-timeout",
                }
                parse_errors = {
                    0: "ok", 1: "too-short", 2: "bad-sync", 3: "bad-pid",
                    4: "bad-length", 5: "bad-crc5", 6: "bad-crc16",
                }
                raw_len = payload[2]
                irq, rx_pc, edge_pc = payload[3:6]
                raw = payload[6 : 6 + min(raw_len, len(payload) - 6)]
                extra = (
                    f" rx={kinds.get(payload[0], payload[0])}"
                    f" parse={parse_errors.get(payload[1], payload[1])}"
                    f" len={raw_len} irq=0x{irq:02x} pc={rx_pc}/{edge_pc}"
                    f" data={raw.hex(' ')}"
                )
            elif payload:
                extra = " data=" + payload.hex(" ")
            print(
                f"#{sequence:<6} t={timestamp:>10}us "
                f"{EVENTS.get(event, f'EVENT_{event}'):<18} "
                f"speed={SPEEDS.get(speed, speed):<10} "
                f"stage={STAGES.get(stage, stage)} "
                f"error={ERRORS.get(error, error)} ep={endpoint}{extra}",
                flush=True,
            )
    except KeyboardInterrupt:
        return 0
    finally:
        os.close(fd)


if __name__ == "__main__":
    sys.exit(main())

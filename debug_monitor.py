"""Windows monitor for the RP2350 bridge's vendor HID debug interface.

Install once with:  py -m pip install hidapi
Run with:           py debug_monitor.py
"""

from __future__ import annotations

import sys
import argparse
import time

try:
    import hid
except ImportError:
    print("Missing dependency. Run: py -m pip install hidapi")
    raise SystemExit(2)


VID = 0x1209
PID = 0x2350

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


def find_debug_path():
    devices = hid.enumerate(VID, PID)
    for item in devices:
        if item.get("usage_page") == 0xFF00 or item.get("interface_number") == 3:
            return item["path"], item
    return None, devices


def normalize(data: list[int]) -> bytes | None:
    raw = bytes(data)
    if raw[:4] == b"PDBG":
        return raw
    # HIDAPI on Windows commonly prepends report ID 0 for descriptors without IDs.
    if len(raw) >= 65 and raw[0] == 0 and raw[1:5] == b"PDBG":
        return raw[1:]
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=float, default=0, help="Exit after this many seconds")
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--bootsel", action="store_true", help="Reboot the board into BOOTSEL")
    action.add_argument("--reset", action="store_true", help="Perform a normal software reboot")
    args = parser.parse_args()
    path, info = find_debug_path()
    if path is None:
        print("Debug HID interface not found. Flash the debug firmware and reconnect Type-C.")
        print("Detected interfaces:", info)
        return 1

    dev = hid.device()
    dev.open_path(path)
    if args.bootsel or args.reset:
        command = b"PBOOT" if args.bootsel else b"PRESET"
        report = bytes([0]) + command + bytes(64 - len(command))
        written = dev.send_feature_report(report)
        print(f"Sent {command.decode()} ({written} bytes).", flush=True)
        time.sleep(0.7)
        dev.close()
        return 0
    print("Connected:", info.get("product_string", "RP2350 bridge"), flush=True)
    print("Waiting for telemetry; press Ctrl+C to stop.\n", flush=True)
    deadline = time.monotonic() + args.seconds if args.seconds > 0 else None
    try:
        while True:
            if deadline is not None and time.monotonic() >= deadline:
                return 0
            report = normalize(dev.read(65, 1000))
            if report is None:
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
        dev.close()


if __name__ == "__main__":
    sys.exit(main())

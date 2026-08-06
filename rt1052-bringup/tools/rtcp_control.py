#!/usr/bin/env python3
"""Send runtime control commands to the RT1052 HID bridge over UDP."""

from __future__ import annotations

import argparse
from decimal import Decimal, InvalidOperation, ROUND_HALF_UP
import secrets
import socket
import struct


MAGIC = b"RTCP"
VERSION = 1
DEFAULT_PORT = 1052
ACK_KIND = 0x80
SET_SENSITIVITY_KIND = 6
MIN_SENSITIVITY_MILLI = 100
MAX_SENSITIVITY_MILLI = 10_000
HEADER = struct.Struct("<4sBBBBI")


def sensitivity_to_milli(value: str) -> int:
    try:
        sensitivity = Decimal(value)
    except InvalidOperation as error:
        raise ValueError(f"invalid sensitivity: {value}") from error
    if not sensitivity.is_finite():
        raise ValueError("sensitivity must be finite")
    milli = int((sensitivity * 1_000).to_integral_value(rounding=ROUND_HALF_UP))
    if not MIN_SENSITIVITY_MILLI <= milli <= MAX_SENSITIVITY_MILLI:
        raise ValueError("sensitivity must be between 0.100 and 10.000")
    return milli


def encode_sensitivity_command(sequence: int, sensitivity_milli: int) -> bytes:
    if not 0 <= sequence <= 0xFFFF_FFFF:
        raise ValueError("sequence must fit in u32")
    if not MIN_SENSITIVITY_MILLI <= sensitivity_milli <= MAX_SENSITIVITY_MILLI:
        raise ValueError("sensitivity_milli is out of range")
    payload = struct.pack("<H", sensitivity_milli)
    return HEADER.pack(
        MAGIC,
        VERSION,
        SET_SENSITIVITY_KIND,
        len(payload),
        0,
        sequence,
    ) + payload


def decode_ack(datagram: bytes, sequence: int, command_kind: int) -> int:
    if len(datagram) != HEADER.size + 2:
        raise ValueError(f"invalid ACK length: {len(datagram)}")
    magic, version, kind, payload_len, flags, received_sequence = HEADER.unpack_from(datagram)
    if magic != MAGIC or version != VERSION or kind != ACK_KIND or flags != 0:
        raise ValueError("invalid ACK header")
    if payload_len != 2 or received_sequence != sequence:
        raise ValueError("ACK does not match the request")
    acknowledged_kind, status = datagram[HEADER.size :]
    if acknowledged_kind != command_kind:
        raise ValueError("ACK command kind does not match the request")
    return status


def send_command(
    host: str,
    port: int,
    datagram: bytes,
    *,
    sequence: int,
    command_kind: int,
    timeout: float,
    retries: int,
) -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
        client.settimeout(timeout)
        client.connect((host, port))
        for attempt in range(1, retries + 1):
            client.send(datagram)
            try:
                response = client.recv(64)
            except TimeoutError:
                if attempt == retries:
                    raise TimeoutError(
                        f"no RTCP ACK from {host}:{port} after {retries} attempts"
                    ) from None
                continue
            status = decode_ack(response, sequence, command_kind)
            if status != 0:
                raise RuntimeError(f"RT1052 rejected command with status {status}")
            return


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Set the in-game sensitivity used to scale the RT1052 recoil trajectory. "
            "Physical mouse movement remains 1:1."
        )
    )
    parser.add_argument("host", help="RT1052 DHCP IPv4 address")
    parser.add_argument("command", choices=("sensitivity",))
    parser.add_argument("value", help="game sensitivity, for example 1.5")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--timeout", type=float, default=0.5)
    parser.add_argument("--retries", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.port <= 65_535:
        parser.error("--port must be between 1 and 65535")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    if args.retries <= 0:
        parser.error("--retries must be positive")
    return args


def main() -> int:
    args = parse_args()
    try:
        sensitivity_milli = sensitivity_to_milli(args.value)
        sequence = secrets.randbits(32)
        datagram = encode_sensitivity_command(sequence, sensitivity_milli)
        send_command(
            args.host,
            args.port,
            datagram,
            sequence=sequence,
            command_kind=SET_SENSITIVITY_KIND,
            timeout=args.timeout,
            retries=args.retries,
        )
    except (OSError, RuntimeError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    print(
        f"RT1052 {args.host}:{args.port} sensitivity="
        f"{sensitivity_milli / 1_000:.3f} ACK"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

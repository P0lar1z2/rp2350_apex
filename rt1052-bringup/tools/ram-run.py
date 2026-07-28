#!/usr/bin/env python3
"""Load and run an RT1052 ITCM ELF without a post-download reset."""

from __future__ import annotations

import argparse
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

GDB_REG_SP = 13
GDB_REG_PC = 15
GDB_REG_MSP = 16
# probe-rs 0.32 exposes MSP and PSP before XPSR in its Cortex-M target XML.
GDB_REG_XPSR = 18


class Rsp:
    def __init__(self, stream: socket.socket) -> None:
        self.stream = stream
        self.no_ack = False

    @staticmethod
    def packet(payload: bytes) -> bytes:
        return b"$" + payload + b"#" + f"{sum(payload) & 0xff:02x}".encode()

    @staticmethod
    def decode(payload: bytes) -> bytes:
        result = bytearray()
        index = 0
        while index < len(payload):
            byte = payload[index]
            if byte == ord("}"):
                index += 1
                result.append(payload[index] ^ 0x20)
            elif byte == ord("*"):
                index += 1
                result.extend([result[-1]] * (payload[index] - 29))
            else:
                result.append(byte)
            index += 1
        return bytes(result)

    def response(self) -> bytes:
        while True:
            byte = self.stream.recv(1)
            if not byte:
                raise RuntimeError("probe-rs GDB server disconnected")
            if byte in (b"+", b"-") or byte != b"$":
                continue
            encoded = bytearray()
            while (byte := self.stream.recv(1)) != b"#":
                if not byte:
                    raise RuntimeError("truncated GDB response")
                encoded.extend(byte)
            checksum = self.stream.recv(2)
            if int(checksum, 16) != (sum(encoded) & 0xFF):
                # probe-rs/gdbstub 0.32 terminates the whole server when a
                # client sends NACK because it cannot retransmit. This is a
                # localhost TCP stream, so accept the framed packet and let
                # command-level validation catch an unusable payload.
                print("warning: accepting RSP packet with checksum mismatch", file=sys.stderr)
            if not self.no_ack:
                self.stream.sendall(b"+")
            return self.decode(bytes(encoded))

    def command(self, payload: str) -> bytes:
        self.stream.sendall(self.packet(payload.encode()))
        response = self.response()
        if response.startswith(b"E"):
            raise RuntimeError(f"GDB command failed: {payload[:20]}: {response!r}")
        return response

    def start_no_ack(self) -> None:
        if self.command("QStartNoAckMode") != b"OK":
            raise RuntimeError("GDB server rejected no-ack mode")
        self.no_ack = True

    def write(self, address: int, data: bytes) -> None:
        for offset in range(0, len(data), 256):
            part = data[offset : offset + 256]
            response = self.command(
                f"M{address + offset:x},{len(part):x}:{part.hex()}"
            )
            if response != b"OK":
                raise RuntimeError(f"memory write failed: {response!r}")

    def read(self, address: int, length: int) -> bytes:
        return bytes.fromhex(self.command(f"m{address:x},{length:x}").decode())

    def set_register(self, number: int, value: int) -> None:
        encoded = struct.pack("<I", value).hex()
        if self.command(f"P{number:x}={encoded}") != b"OK":
            raise RuntimeError(f"register {number} write failed")

    def get_register(self, number: int) -> int:
        encoded = self.command(f"p{number:x}")
        return int.from_bytes(bytes.fromhex(encoded.decode()), "little")

    def resume(self) -> None:
        self.stream.sendall(self.packet(b"c"))

    def halt(self) -> bytes:
        self.stream.sendall(b"\x03")
        return self.response()


def elf_image(path: Path) -> tuple[int, int, list[tuple[int, bytes]]]:
    image = path.read_bytes()
    if image[:6] != b"\x7fELF\x01\x01":
        raise RuntimeError("expected a 32-bit little-endian ELF")
    entry = struct.unpack_from("<I", image, 24)[0]
    phoff = struct.unpack_from("<I", image, 28)[0]
    phentsize, phnum = struct.unpack_from("<HH", image, 42)
    segments: list[tuple[int, bytes]] = []
    for index in range(phnum):
        header = phoff + index * phentsize
        p_type, offset, _, load_address, file_size, _ = struct.unpack_from(
            "<IIIIII", image, header
        )
        if p_type == 1 and file_size:
            # Cortex-M images can give .data a RAM virtual address and a
            # distinct ITCM load address. Reset copies from p_paddr to
            # p_vaddr, so a RAM-only loader must populate p_paddr just like a
            # flash programmer would.
            segments.append((load_address, image[offset : offset + file_size]))
    initial_sp = struct.unpack_from("<I", segments[0][1], 0)[0]
    return entry, initial_sp, segments


def connect(port: int) -> socket.socket:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            return socket.create_connection(("127.0.0.1", port), timeout=2)
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("probe-rs GDB server did not start")


def elf_symbol(path: Path, name: str) -> int | None:
    try:
        symbols = subprocess.check_output(
            ["arm-none-eabi-nm", "-n", str(path)], text=True
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    for line in symbols.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[-1] == name:
            return int(fields[0], 16)
    return None


def drain_rtt(rsp: Rsp, base: int) -> bytes:
    header = rsp.read(base, 48)
    if not header.startswith(b"SEGGER RTT"):
        return b""
    _, buffer, size, write, read, _ = struct.unpack_from("<6I", header, 24)
    if buffer == 0 or size == 0 or size > 64 * 1024:
        return b""
    if write >= read:
        output = rsp.read(buffer + read, write - read)
    else:
        output = rsp.read(buffer + read, size - read) + rsp.read(buffer, write)
    rsp.write(base + 40, struct.pack("<I", write))
    return output


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("elf", type=Path)
    parser.add_argument("--seconds", type=float, default=3)
    parser.add_argument("--speed", type=int, default=50)
    parser.add_argument("--port", type=int, default=1337)
    parser.add_argument("--no-verify", action="store_true")
    parser.add_argument(
        "--connect-under-reset",
        action="store_true",
        help="ask the probe to hold NRST while attaching",
    )
    args = parser.parse_args()
    entry, initial_sp, segments = elf_image(args.elf)
    rtt_base = elf_symbol(args.elf, "_SEGGER_RTT") or 0x2000_0000

    command = [
        "probe-rs",
        "gdb",
        "--chip",
        "MIMXRT1052CVL5B",
        "--protocol",
        "swd",
        "--reset-halt",
        "--speed",
        str(args.speed),
        "--gdb-connection-string",
        f"127.0.0.1:{args.port}",
    ]
    if args.connect_under_reset:
        command.append("--connect-under-reset")
    server = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    success = False
    try:
        with connect(args.port) as stream:
            rsp = Rsp(stream)
            rsp.command("?")
            rsp.start_no_ack()
            for address, data in segments:
                rsp.write(address, data)
                if args.no_verify:
                    continue
                for offset in range(0, len(data), 256):
                    part = data[offset : offset + 256]
                    if rsp.read(address + offset, len(part)) != part:
                        raise RuntimeError(
                            f"ELF verification failed at 0x{address + offset:08x}"
                        )
            rsp.set_register(GDB_REG_SP, initial_sp)
            rsp.set_register(GDB_REG_MSP, initial_sp)
            rsp.set_register(GDB_REG_PC, entry)
            rsp.set_register(GDB_REG_XPSR, 0x0100_0000)
            rsp.resume()

            # Let the target run continuously. The five-wire fireDAP link is
            # noticeably more reliable when we halt only once for inspection.
            time.sleep(args.seconds)
            rsp.halt()
            output = drain_rtt(rsp, rtt_base)
            if output:
                print(output.decode(errors="replace"), end="", flush=True)
            gpr14 = struct.unpack("<I", rsp.read(0x400A_C038, 4))[0]
            gpr16 = struct.unpack("<I", rsp.read(0x400A_C040, 4))[0]
            gpr17 = struct.unpack("<I", rsp.read(0x400A_C044, 4))[0]
            gpr1 = struct.unpack("<I", rsp.read(0x400A_C004, 4))[0]
            enet_pll = struct.unpack("<I", rsp.read(0x400D_80E0, 4))[0]
            dma = rsp.read(0x2020_0000, 32)
            enet_tx_bd = rsp.read(0x2020_0040, 24)
            rx_frames = []
            for index in range(4):
                length, control, buffer = struct.unpack_from("<HHI", dma, index * 8)
                head = rsp.read(buffer, min(length, 32)) if buffer and length else b""
                rx_frames.append(
                    f"rx{index}=len:{length},ctrl:0x{control:04x},"
                    f"buf:0x{buffer:08x},head:{head.hex()}"
                )
            print(
                f"state: pc=0x{rsp.get_register(GDB_REG_PC):08x} "
                f"sp=0x{rsp.get_register(GDB_REG_SP):08x} "
                f"xpsr=0x{rsp.get_register(GDB_REG_XPSR):08x} "
                f"gpr14=0x{gpr14:08x} gpr16=0x{gpr16:08x} "
                f"gpr17=0x{gpr17:08x} gpr1=0x{gpr1:08x} "
                f"enet_pll=0x{enet_pll:08x} dma={dma.hex()} "
                f"enet_tx_bd={enet_tx_bd.hex()} " + " ".join(rx_frames)
            )
            success = True
    finally:
        server.terminate()
        try:
            server_output, _ = server.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            server.kill()
            server_output, _ = server.communicate()
        if not success:
            print(server_output, file=sys.stderr)

    print(f"RAM run complete: entry=0x{entry:08x}, sp=0x{initial_sp:08x}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Build, back up, program, and verify the RT1052 Pro FlexSPI image.

The board-specific FCB/DCD is preserved from the known-good image already on
the board. Every `flash` operation takes a complete 32 MiB backup first.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess


FLASH_BASE = 0x6000_0000
APP_BASE = 0x6000_2000
FLASH_SIZE = 32 * 1024 * 1024
HEADER_SIZE = APP_BASE - FLASH_BASE
FCB_TAG = 0x4246_4346
IVT_HEADER = 0x4120_00D1
IVT_ADDRESS = 0x6000_1000
BOOT_DATA_ADDRESS = 0x6000_1020
DEFAULT_TARGET = "mimxrt1050_quadspi"

SCRIPT_DIR = Path(__file__).resolve().parent
CRATE_DIR = SCRIPT_DIR.parent
REPO_DIR = CRATE_DIR.parent
ARTIFACT_DIR = CRATE_DIR / ".flash"
ELF_PATH = CRATE_DIR / "target/thumbv7em-none-eabihf/release/hid_bridge"


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, cwd=REPO_DIR, check=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def frequency_hz(value: str) -> int:
    text = value.strip().lower().removesuffix("hz")
    multiplier = 1
    if text.endswith("k"):
        multiplier, text = 1_000, text[:-1]
    elif text.endswith("m"):
        multiplier, text = 1_000_000, text[:-1]
    return int(float(text) * multiplier)


def pyocd_path(requested: str | None) -> str:
    if requested:
        path = shutil.which(requested) if "/" not in requested else requested
    else:
        local_pyocd = CRATE_DIR / ".venv-pyocd/bin/pyocd"
        path = str(local_pyocd) if local_pyocd.is_file() else shutil.which("pyocd")
    if not path or not Path(path).is_file():
        raise SystemExit(
            "pyocd not found; pass --pyocd /path/to/venv/bin/pyocd "
            "(pyOCD 0.45 or newer is recommended)"
        )
    return str(path)


def connection_args(args: argparse.Namespace, command: str, frequency: str) -> list[str]:
    result = [
        pyocd_path(args.pyocd),
        command,
        "--no-config",
        "-t",
        DEFAULT_TARGET,
        "-f",
        frequency,
    ]
    if args.probe:
        result.extend(["-u", args.probe])
    return result


def validate_backup(path: Path) -> None:
    if not path.is_file():
        raise SystemExit(f"backup was not created; check the DAP connection: {path}")
    if path.stat().st_size != FLASH_SIZE:
        raise SystemExit(f"backup must be exactly {FLASH_SIZE} bytes: {path}")
    with path.open("rb") as source:
        header = source.read(HEADER_SIZE)
    word = lambda offset: struct.unpack_from("<I", header, offset)[0]
    checks = {
        "FCB tag": (word(0x0000), FCB_TAG),
        "IVT header": (word(0x1000), IVT_HEADER),
        "IVT entry": (word(0x1004), APP_BASE),
        "IVT self": (word(0x1014), IVT_ADDRESS),
        "boot data pointer": (word(0x1010), BOOT_DATA_ADDRESS),
        "boot start": (word(0x1020), FLASH_BASE),
        "flash size": (word(0x1024), FLASH_SIZE),
    }
    failures = [
        f"{name}: got 0x{actual:08x}, expected 0x{expected:08x}"
        for name, (actual, expected) in checks.items()
        if actual != expected
    ]
    if failures:
        raise SystemExit("invalid RT1052 boot header:\n  " + "\n  ".join(failures))


def backup(args: argparse.Namespace) -> Path:
    ARTIFACT_DIR.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now().astimezone().strftime("%Y%m%d-%H%M%S")
    output = Path(args.output).resolve() if args.output else ARTIFACT_DIR / f"w25q256-{timestamp}.bin"
    if output.exists():
        raise SystemExit(f"refusing to overwrite backup: {output}")
    command = connection_args(args, "commander", "10k") + [
        "-M",
        "under-reset",
        "-c",
        "halt",
        "-c",
        f"set frequency {frequency_hz(args.read_frequency)}",
        "-c",
        f"savemem 0x{FLASH_BASE:08x} 0x{FLASH_SIZE:x} {output}",
        "-c",
        "go",
    ]
    run(command)
    validate_backup(output)
    digest = sha256(output)
    output.with_suffix(".sha256").write_text(f"{digest}  {output.name}\n", encoding="ascii")
    print(f"backup: {output}\nsha256: {digest}")
    return output


def elf_flash_payload(path: Path) -> bytes:
    image = path.read_bytes()
    if image[:6] != b"\x7fELF\x01\x01":
        raise SystemExit(f"expected a 32-bit little-endian ELF: {path}")
    phoff = struct.unpack_from("<I", image, 28)[0]
    phentsize, phnum = struct.unpack_from("<HH", image, 42)
    segments: list[tuple[int, bytes]] = []
    for index in range(phnum):
        header = phoff + index * phentsize
        p_type, offset, _, load_address, file_size, _ = struct.unpack_from(
            "<IIIIII", image, header
        )
        if (
            p_type == 1
            and file_size
            and APP_BASE <= load_address < FLASH_BASE + FLASH_SIZE
        ):
            segments.append((load_address, image[offset : offset + file_size]))
    if not segments or min(address for address, _ in segments) != APP_BASE:
        raise SystemExit("ELF has no FlexSPI application segment at 0x60002000")
    end = max(address + len(data) for address, data in segments)
    payload = bytearray(b"\xff" * (end - APP_BASE))
    for address, data in segments:
        start = address - APP_BASE
        payload[start : start + len(data)] = data
    return bytes(payload)


def build_image(args: argparse.Namespace, backup_path: Path) -> Path:
    validate_backup(backup_path)
    ARTIFACT_DIR.mkdir(parents=True, exist_ok=True)
    run(
        [
            "cargo",
            "build",
            "--manifest-path",
            str(CRATE_DIR / "Cargo.toml"),
            "--release",
            "--target",
            "thumbv7em-none-eabihf",
            "--features",
            "nxp-host,nxp-device,flash-xip",
            "--bin",
            "hid_bridge",
        ]
    )
    app = elf_flash_payload(ELF_PATH)
    app_path = ARTIFACT_DIR / "hid_bridge-xip-app.bin"
    app_path.write_bytes(app)
    if len(app) < 8:
        raise SystemExit("XIP application binary is missing its vector table")
    initial_sp, reset = struct.unpack_from("<II", app)
    if not (0x2000_0000 <= initial_sp <= 0x2002_0000):
        raise SystemExit(f"unexpected initial SP 0x{initial_sp:08x}")
    if not (APP_BASE <= (reset & ~1) < FLASH_BASE + FLASH_SIZE) or reset & 1 == 0:
        raise SystemExit(f"unexpected reset vector 0x{reset:08x}")

    header = bytearray(backup_path.read_bytes()[:HEADER_SIZE])
    struct.pack_into("<I", header, 0x1004, APP_BASE)
    struct.pack_into("<I", header, 0x1010, BOOT_DATA_ADDRESS)
    struct.pack_into("<I", header, 0x1014, IVT_ADDRESS)
    struct.pack_into("<I", header, 0x1020, FLASH_BASE)
    struct.pack_into("<I", header, 0x1024, FLASH_SIZE)

    image_path = ARTIFACT_DIR / "hid_bridge-rt1052-boot.bin"
    image_path.write_bytes(header + app)
    if image_path.stat().st_size > 8 * 1024 * 1024:
        raise SystemExit("image exceeds pyOCD's 8 MiB quad-SPI programming region")

    manifest = {
        "image": image_path.name,
        "image_size": image_path.stat().st_size,
        "image_sha256": sha256(image_path),
        "header_source": str(backup_path),
        "header_source_sha256": sha256(backup_path),
        "flash_base": f"0x{FLASH_BASE:08x}",
        "application_base": f"0x{APP_BASE:08x}",
        "initial_sp": f"0x{initial_sp:08x}",
        "reset_vector": f"0x{reset:08x}",
    }
    (ARTIFACT_DIR / "hid_bridge-rt1052-boot.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(manifest, indent=2))
    return image_path


def verify_image(args: argparse.Namespace, image_path: Path) -> None:
    if not image_path.is_file():
        raise SystemExit(f"image not found: {image_path}")
    verify_path = ARTIFACT_DIR / "hid_bridge-rt1052-readback.bin"
    command = connection_args(args, "commander", "10k") + [
        "-M",
        "under-reset",
        "-c",
        "halt",
        "-c",
        f"set frequency {frequency_hz(args.read_frequency)}",
        "-c",
        f"savemem 0x{FLASH_BASE:08x} 0x{image_path.stat().st_size:x} {verify_path}",
    ]
    run(command)
    if not verify_path.is_file():
        raise SystemExit("Flash readback was not created; reconnect DAP under reset and retry")
    expected = sha256(image_path)
    actual = sha256(verify_path)
    if actual != expected:
        raise SystemExit(f"Flash verify failed: expected {expected}, read {actual}")
    print(f"verified {image_path.stat().st_size} bytes, sha256 {actual}")


def boot_target(args: argparse.Namespace) -> None:
    command = connection_args(args, "commander", "10k") + [
        "-M",
        "under-reset",
        "-c",
        "go",
    ]
    run(command)


def flash(args: argparse.Namespace) -> None:
    backup_path = backup(args)
    image_path = build_image(args, backup_path)
    if not args.yes:
        raise SystemExit(
            f"backup and image are ready at {ARTIFACT_DIR}; rerun with `flash --yes` to program"
        )
    command = connection_args(args, "flash", args.program_frequency) + [
        "-M",
        "halt",
        "--format",
        "bin",
        "--erase",
        "sector",
        "--base-address",
        f"0x{FLASH_BASE:08x}",
        "--no-reset",
        str(image_path),
    ]
    run(command)
    verify_image(args, image_path)
    boot_target(args)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--pyocd", help="pyocd executable (or set PYOCD)", default=os.getenv("PYOCD"))
    result.add_argument("--probe", help="CMSIS-DAP serial (or set PYOCD_PROBE)", default=os.getenv("PYOCD_PROBE"))
    result.add_argument("--read-frequency", default="10m", help="SWD rate for backup/verify")
    result.add_argument("--program-frequency", default="50k", help="SWD rate for programming")
    sub = result.add_subparsers(dest="command", required=True)
    backup_parser = sub.add_parser("backup", help="read and validate the entire 32 MiB Flash")
    backup_parser.add_argument("--output")
    build_parser = sub.add_parser("build", help="build a boot image using a validated backup header")
    build_parser.add_argument("backup", type=Path)
    flash_parser = sub.add_parser("flash", help="back up, build, program, and verify")
    flash_parser.add_argument("--output", help="path for the pre-flash backup")
    flash_parser.add_argument("--yes", action="store_true", help="confirm sector erase/programming")
    verify_parser = sub.add_parser("verify", help="read back an existing image and boot it")
    verify_parser.add_argument("image", type=Path)
    return result


def main() -> None:
    args = parser().parse_args()
    if args.command == "backup":
        backup(args)
    elif args.command == "build":
        build_image(args, args.backup.resolve())
    elif args.command == "flash":
        flash(args)
    elif args.command == "verify":
        image_path = args.image.resolve()
        verify_image(args, image_path)
        boot_target(args)
    else:
        raise AssertionError(args.command)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from error

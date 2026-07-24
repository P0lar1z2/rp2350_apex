"""Reboot dynamic-clone firmware into the RP2350 ROM BOOTSEL interface.

The request is intentionally not advertised in any USB descriptor, so it does
not change the cloned device identity. Run as a user with permission to open
the USB device (or via sudo) and specify the currently cloned VID/PID. If more
than one matching device exists, also pass --bus and --address from ``lsusb``.
"""

from __future__ import annotations

import argparse
import ctypes
import ctypes.util


class DeviceDescriptor(ctypes.Structure):
    _fields_ = [
        ("bLength", ctypes.c_uint8),
        ("bDescriptorType", ctypes.c_uint8),
        ("bcdUSB", ctypes.c_uint16),
        ("bDeviceClass", ctypes.c_uint8),
        ("bDeviceSubClass", ctypes.c_uint8),
        ("bDeviceProtocol", ctypes.c_uint8),
        ("bMaxPacketSize0", ctypes.c_uint8),
        ("idVendor", ctypes.c_uint16),
        ("idProduct", ctypes.c_uint16),
        ("bcdDevice", ctypes.c_uint16),
        ("iManufacturer", ctypes.c_uint8),
        ("iProduct", ctypes.c_uint8),
        ("iSerialNumber", ctypes.c_uint8),
        ("bNumConfigurations", ctypes.c_uint8),
    ]


def parse_number(value: str) -> int:
    return int(value, 0)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--vid", type=parse_number, required=True, help="e.g. 0x046d")
    parser.add_argument("--pid", type=parse_number, required=True, help="e.g. 0xc52b")
    parser.add_argument("--bus", type=int)
    parser.add_argument("--address", type=int)
    args = parser.parse_args()

    library = ctypes.util.find_library("usb-1.0")
    if library is None:
        raise SystemExit("libusb-1.0 was not found")
    usb = ctypes.CDLL(library)
    usb.libusb_init.argtypes = [ctypes.POINTER(ctypes.c_void_p)]
    usb.libusb_init.restype = ctypes.c_int
    usb.libusb_get_device_list.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p)),
    ]
    usb.libusb_get_device_list.restype = ctypes.c_ssize_t
    usb.libusb_get_device_descriptor.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(DeviceDescriptor),
    ]
    usb.libusb_get_device_descriptor.restype = ctypes.c_int
    usb.libusb_get_bus_number.argtypes = [ctypes.c_void_p]
    usb.libusb_get_bus_number.restype = ctypes.c_uint8
    usb.libusb_get_device_address.argtypes = [ctypes.c_void_p]
    usb.libusb_get_device_address.restype = ctypes.c_uint8
    usb.libusb_open.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
    usb.libusb_open.restype = ctypes.c_int
    usb.libusb_control_transfer.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint8,
        ctypes.c_uint8,
        ctypes.c_uint16,
        ctypes.c_uint16,
        ctypes.POINTER(ctypes.c_ubyte),
        ctypes.c_uint16,
        ctypes.c_uint,
    ]
    usb.libusb_control_transfer.restype = ctypes.c_int
    usb.libusb_close.argtypes = [ctypes.c_void_p]
    usb.libusb_free_device_list.argtypes = [ctypes.POINTER(ctypes.c_void_p), ctypes.c_int]
    usb.libusb_exit.argtypes = [ctypes.c_void_p]
    context = ctypes.c_void_p()
    if usb.libusb_init(ctypes.byref(context)) != 0:
        raise SystemExit("libusb_init failed")
    devices = ctypes.POINTER(ctypes.c_void_p)()
    count = usb.libusb_get_device_list(context, ctypes.byref(devices))
    matches: list[tuple[ctypes.c_void_p, int, int]] = []
    try:
        for index in range(max(0, count)):
            device = devices[index]
            descriptor = DeviceDescriptor()
            if usb.libusb_get_device_descriptor(device, ctypes.byref(descriptor)) != 0:
                continue
            bus = usb.libusb_get_bus_number(device)
            address = usb.libusb_get_device_address(device)
            if descriptor.idVendor != args.vid or descriptor.idProduct != args.pid:
                continue
            if args.bus is not None and bus != args.bus:
                continue
            if args.address is not None and address != args.address:
                continue
            matches.append((device, bus, address))
        if len(matches) != 1:
            found = ", ".join(f"Bus {bus:03d} Device {address:03d}" for _, bus, address in matches)
            raise SystemExit(f"expected exactly one matching device; found {len(matches)}: {found}")
        device, bus, address = matches[0]
        handle = ctypes.c_void_p()
        result = usb.libusb_open(device, ctypes.byref(handle))
        if result != 0:
            raise SystemExit(f"libusb_open failed ({result}); check device permissions")
        try:
            result = usb.libusb_control_transfer(
                handle,
                0x40,  # Host-to-device, vendor, device recipient.
                0x5A,
                0x2350,
                0x5546,
                None,
                0,
                1000,
            )
            if result < 0:
                raise SystemExit(f"control transfer failed ({result})")
            print(f"Sent BOOTSEL request to Bus {bus:03d} Device {address:03d}")
        finally:
            usb.libusb_close(handle)
    finally:
        usb.libusb_free_device_list(devices, 1)
        usb.libusb_exit(context)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

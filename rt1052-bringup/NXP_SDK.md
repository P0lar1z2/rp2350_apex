# Pinned NXP MCUXpresso SDK sources

The USB Host integration follows MCUXpresso SDK `26.06.00 LTS`. Complete vendor
trees live under the repository-level `.vendor/` directory and are ignored by
Git. The tracked firmware contains only the local configuration, bare-metal OSA
adapter, and Rust FFI.

| Local directory | Upstream repository | Pinned revision |
| --- | --- | --- |
| `.vendor/mcuxsdk-core` | `nxp-mcuxpresso/mcuxsdk-core` | `a910e7645d2d809a3431e1d5f42fca1cdeee69c9` |
| `.vendor/mcux-devices-rt` | `nxp-mcuxpresso/mcux-devices-rt` | `3248d6d5f6bcb7b89fc43abd81700c7572c61bf0` |
| `.vendor/mcux-sdk-middleware-usb` | `nxp-mcuxpresso/mcux-sdk-middleware-usb` | `2289f6c8ce0d07e57421ed9b50e2a82d0c38568b` |
| `.vendor/mcu-sdk-cmsis` | `nxp-mcuxpresso/mcu-sdk-cmsis` | `e07cca54712c65a938f41a3e72fbfcb20e2f864a` |
| `.vendor/mcux-component` | `nxp-mcuxpresso/mcux-component` | `c4fba0f97e0c889b9235b53c686d2d2dcc5defa4` |

These revisions come from the official manifest branch
`release/26.06.00-lts`. The USB stack reports version `2.12.2` from
`middleware/usb/include/usb.h`.

Only these NXP sources are compiled for the first probe:

- common and RT1052 clock drivers;
- USB PHY;
- Host core, enumeration framework, EHCI;
- Hub and HID class drivers.

All other middleware and RTOS code remains outside the firmware build.

#![no_std]

/// Marks firmware that relies on the RT1052 reset-time default RAM partition.
///
/// Keeping this call in binaries also retains this crate's native-link
/// metadata when the `nxp-host` feature builds the NXP USB C archive.
#[inline(always)]
pub fn use_nxp_default_flexram() {}

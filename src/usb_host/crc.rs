/// USB token CRC-5 (polynomial `x^5 + x^2 + 1`).
///
/// Only the low 11 bits of `token` are included, least-significant bit first.
pub const fn crc5_token(mut token: u16) -> u8 {
    let mut crc = 0x1fu8;
    let mut bit = 0;
    while bit < 11 {
        let feedback = (crc ^ token as u8) & 1;
        crc >>= 1;
        if feedback != 0 {
            // Reflected representation of x^5 + x^2 + 1.
            crc ^= 0x14;
        }
        token >>= 1;
        bit += 1;
    }
    crc ^ 0x1f
}

/// One byte of USB CRC-16 state update, without the final complement.
///
/// Exposed so receivers can maintain a rolling CRC while bytes stream in and
/// send the handshake immediately at EOP instead of re-scanning the packet.
pub const fn crc16_step(mut crc: u16, byte: u8) -> u16 {
    crc ^= byte as u16;
    let mut bit = 0;
    while bit < 8 {
        crc = if crc & 1 != 0 {
            (crc >> 1) ^ 0xa001
        } else {
            crc >> 1
        };
        bit += 1;
    }
    crc
}

/// USB data CRC-16 (polynomial `x^16 + x^15 + x^2 + 1`).
///
/// The returned value is transmitted low byte first.
pub fn crc16_usb(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &byte in bytes {
        crc = crc16_step(crc, byte);
    }
    crc ^ 0xffff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_standard_check_value() {
        // CRC-16/USB's standard catalogue check vector.
        assert_eq!(crc16_usb(b"123456789"), 0xb4c8);
        assert_eq!(crc16_usb(&[]), 0x0000);
    }

    #[test]
    fn token_crc_uses_exactly_eleven_bits() {
        for token in 0..=0x07ff {
            assert_eq!(crc5_token(token), reference_crc5(token));
            assert_eq!(crc5_token(token | 0xf800), crc5_token(token));
        }
    }

    // Independent formulation used only by tests. It mirrors polynomial long
    // division, rather than the shift-register implementation above.
    fn reference_crc5(token: u16) -> u8 {
        let mut value = (token ^ 0x001f) as u32;
        let mut crc = 0u8;
        for _ in 0..11 {
            let input = (value & 1) as u8;
            value >>= 1;
            let feedback = input ^ ((crc >> 4) & 1);
            crc = (crc << 1) & 0x1f;
            if feedback != 0 {
                crc ^= 0x05;
            }
        }
        crc.reverse_bits() >> 3 ^ 0x1f
    }
}

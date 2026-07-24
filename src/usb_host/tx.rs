/// Two-bit symbols consumed by the PIO transmitter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LineSymbol {
    Se0 = 0,
    K = 1,
    Complement = 2,
    J = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    OutputTooSmall,
}

/// Encode packet bytes as NRZI line symbols, including USB bit stuffing and
/// the two-symbol EOP. Four symbols are packed MSB-first into each output byte.
///
/// `packet` must already include SYNC, PID and any packet CRC. The trailing
/// padding uses K symbols and is ignored after EOP by the PIO program.
pub fn encode_tx_packet(packet: &[u8], output: &mut [u8]) -> Result<usize, EncodeError> {
    let mut packer = SymbolPacker::new(output);
    let mut state_is_k = true;
    let mut consecutive_ones = 0u8;

    for &byte in packet {
        let mut bits = byte;
        for _ in 0..8 {
            if bits & 1 == 0 {
                state_is_k = !state_is_k;
                packer.push(if state_is_k {
                    LineSymbol::K
                } else {
                    LineSymbol::J
                })?;
                consecutive_ones = 0;
            } else {
                packer.push(if state_is_k {
                    LineSymbol::K
                } else {
                    LineSymbol::J
                })?;
                consecutive_ones += 1;
                if consecutive_ones == 6 {
                    state_is_k = !state_is_k;
                    packer.push(if state_is_k {
                        LineSymbol::K
                    } else {
                        LineSymbol::J
                    })?;
                    consecutive_ones = 0;
                }
            }
            bits >>= 1;
        }
    }

    packer.push(LineSymbol::Se0)?;
    packer.push(LineSymbol::Complement)?;
    packer.finish_with(LineSymbol::K)
}

struct SymbolPacker<'a> {
    output: &'a mut [u8],
    byte_index: usize,
    symbols_in_byte: u8,
}

impl<'a> SymbolPacker<'a> {
    fn new(output: &'a mut [u8]) -> Self {
        Self {
            output,
            byte_index: 0,
            symbols_in_byte: 0,
        }
    }

    fn push(&mut self, symbol: LineSymbol) -> Result<(), EncodeError> {
        if self.byte_index >= self.output.len() {
            return Err(EncodeError::OutputTooSmall);
        }
        if self.symbols_in_byte == 0 {
            self.output[self.byte_index] = 0;
        }
        self.output[self.byte_index] = (self.output[self.byte_index] << 2) | symbol as u8;
        self.symbols_in_byte += 1;
        if self.symbols_in_byte == 4 {
            self.symbols_in_byte = 0;
            self.byte_index += 1;
        }
        Ok(())
    }

    fn finish_with(mut self, padding: LineSymbol) -> Result<usize, EncodeError> {
        while self.symbols_in_byte != 0 {
            self.push(padding)?;
        }
        Ok(self.byte_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_has_expected_nrzi_symbols() {
        let mut encoded = [0u8; 8];
        let len = encode_tx_packet(&[0x80], &mut encoded).unwrap();
        // SYNC bits (00000001 LSB-first), then SE0, complement and K padding.
        assert_eq!(&encoded[..len], &[0xdd, 0xdf, 0x25]);
    }

    #[test]
    fn inserts_zero_after_six_one_bits() {
        let mut encoded = [0u8; 16];
        let len = encode_tx_packet(&[0xff], &mut encoded).unwrap();
        // Eight data bits become nine symbols due to one stuffed transition,
        // followed by EOP and one padding symbol: twelve symbols total.
        assert_eq!(len, 3);
        assert_eq!(unpack(&encoded[..len])[0..9], [1, 1, 1, 1, 1, 1, 3, 3, 3]);
    }

    #[test]
    fn reports_short_output_without_panicking() {
        assert_eq!(
            encode_tx_packet(&[0x80], &mut [0u8; 2]),
            Err(EncodeError::OutputTooSmall)
        );
    }

    fn unpack(bytes: &[u8]) -> [u8; 64] {
        let mut result = [0u8; 64];
        let mut index = 0;
        for &byte in bytes {
            for shift in [6, 4, 2, 0] {
                result[index] = (byte >> shift) & 3;
                index += 1;
            }
        }
        result
    }
}

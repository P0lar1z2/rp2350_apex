from __future__ import annotations

import struct
import unittest

from rtcp_control import (
    ACK_KIND,
    HEADER,
    MAGIC,
    SET_SENSITIVITY_KIND,
    VERSION,
    decode_ack,
    encode_sensitivity_command,
    sensitivity_to_milli,
)


class RtcpControlTests(unittest.TestCase):
    def test_sensitivity_conversion_uses_thousandths(self) -> None:
        self.assertEqual(sensitivity_to_milli("1.5"), 1_500)
        self.assertEqual(sensitivity_to_milli("0.1004"), 100)
        self.assertEqual(sensitivity_to_milli("0.1005"), 101)
        with self.assertRaises(ValueError):
            sensitivity_to_milli("0")
        with self.assertRaises(ValueError):
            sensitivity_to_milli("nan")

    def test_command_matches_rtcp_wire_format(self) -> None:
        datagram = encode_sensitivity_command(0x1234_5678, 1_500)
        self.assertEqual(datagram[:4], MAGIC)
        self.assertEqual(datagram[4:8], bytes((VERSION, SET_SENSITIVITY_KIND, 2, 0)))
        self.assertEqual(datagram[8:12], struct.pack("<I", 0x1234_5678))
        self.assertEqual(datagram[12:], struct.pack("<H", 1_500))

    def test_ack_is_correlated_with_request(self) -> None:
        sequence = 9
        ack = HEADER.pack(MAGIC, VERSION, ACK_KIND, 2, 0, sequence) + bytes(
            (SET_SENSITIVITY_KIND, 0)
        )
        self.assertEqual(decode_ack(ack, sequence, SET_SENSITIVITY_KIND), 0)
        with self.assertRaises(ValueError):
            decode_ack(ack, sequence + 1, SET_SENSITIVITY_KIND)


if __name__ == "__main__":
    unittest.main()

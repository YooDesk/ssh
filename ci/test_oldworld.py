"""Keep new-world ELF objects and newer glibc out of ABI 1.0 archives."""
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

from package import verify_binary


class OldworldBinaryTests(unittest.TestCase):
    def test_abi_and_runtime_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'plugin.so'
            for flags, glibc, valid in ((3, '2.28', True), (0x43, '2.28', False), (3, '2.29', False)):
                with self.subTest(flags=flags, glibc=glibc):
                    data = bytearray(64)
                    data[:6] = b'\x7fELF\x02\x01'
                    struct.pack_into('<H', data, 18, 258)
                    struct.pack_into('<I', data, 48, flags)
                    path.write_bytes(data)
                    with patch('package.subprocess.check_output', return_value='GLIBC_' + glibc):
                        if valid:
                            verify_binary(path, 'linux-loong64-oldworld')
                        else:
                            with self.assertRaises(ValueError):
                                verify_binary(path, 'linux-loong64-oldworld')

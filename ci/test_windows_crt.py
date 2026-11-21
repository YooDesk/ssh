"""Audit ordinary and delayed DLL imports in both Windows image formats."""
import struct
import tempfile
import unittest
from pathlib import Path

from windows_crt import imports, verify


def image(magic, dll, delayed=False):
    blob = bytearray(2048)
    blob[:2] = b'MZ'
    struct.pack_into('<I', blob, 0x3c, 128)
    blob[128:132] = b'PE\0\0'
    struct.pack_into('<H', blob, 134, 1)
    size = 240 if magic == 0x20b else 224
    struct.pack_into('<H', blob, 148, size)
    optional = 152
    struct.pack_into('<H', blob, optional, magic)
    base = 0x400000
    if magic == 0x20b:
        struct.pack_into('<Q', blob, optional+24, base)
    else:
        struct.pack_into('<I', blob, optional+28, base)
    directories = optional + (112 if magic == 0x20b else 96)
    struct.pack_into('<I', blob, directories + (13 if delayed else 1)*8, 0x1000)
    struct.pack_into('<IIII', blob, optional+size+8, 1024, 0x1000, 1024, 512)
    if delayed:
        # Exercise the legacy VA representation used by 32-bit delay imports.
        struct.pack_into('<IIIIIIII', blob, 512, 0, base+0x1100, 0, 0, 0, 0, 0, 0)
    else:
        struct.pack_into('<IIIII', blob, 512, 0, 0, 0, 0x1100, 0)
    name = dll.encode('ascii') + b'\0'
    blob[768:768+len(name)] = name
    return bytes(blob)


class WindowsImportsTests(unittest.TestCase):
    def test_system_imports_in_both_formats(self):
        for magic in (0x10b, 0x20b):
            for delayed in (False, True):
                with self.subTest(magic=magic, delayed=delayed):
                    self.assertEqual(imports(image(magic, 'KERNEL32.dll', delayed)), ['kernel32.dll'])

    def test_crt_and_external_imports_remain_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'plugin.dll'
            for magic in (0x10b, 0x20b):
                for delayed in (False, True):
                    for dll in ('ucrtbase.dll', 'vcruntime140.dll', 'unexpected.dll'):
                        with self.subTest(magic=magic, delayed=delayed, dll=dll):
                            path.write_bytes(image(magic, dll, delayed))
                            with self.assertRaises(ValueError):
                                verify(path)

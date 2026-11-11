"""Unit tests for the Mach-O checks used by the macOS release archives."""
import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from macho import CPU_TYPES, inspect, verify  # noqa: E402


def image(cputype=CPU_TYPES['macos-arm64'], libraries=(), rpaths=()):
    """Assemble a minimal thin Mach-O header with the requested load commands."""
    size = 32
    body = b''
    commands = []
    for name in libraries:
        payload = name.encode() + b'\0'
        payload += b'\0' * (-len(payload) % 8)
        command_size = 24 + len(payload)
        commands.append(struct.pack('<IIIIII', 0xC, command_size, 24, 2, 0x10000, 0x10000) + payload)
        size += command_size
    for name in rpaths:
        payload = name.encode() + b'\0'
        payload += b'\0' * (-len(payload) % 8)
        command_size = 12 + len(payload)
        commands.append(struct.pack('<III', 0x8000001C, command_size, 12) + payload)
        size += command_size
    header = struct.pack('<IIIIIIII', 0xFEEDFACF, cputype, 0, 2, len(commands), size, 0, 0)
    return header + b''.join(commands)


class MachoTests(unittest.TestCase):
    def write(self, data):
        directory = tempfile.mkdtemp(prefix='macho-')
        path = Path(directory) / 'binary'
        path.write_bytes(data)
        self.addCleanup(lambda: __import__('shutil').rmtree(directory, ignore_errors=True))
        return path

    def test_system_frameworks_and_libraries_are_accepted(self):
        path = self.write(image(libraries=[
            '/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation',
            '/usr/lib/libSystem.B.dylib',
        ]))
        result = verify(path, 'macos-arm64')
        self.assertEqual(result['cputype'], CPU_TYPES['macos-arm64'])

    def test_homebrew_or_bundled_libraries_are_rejected(self):
        path = self.write(image(libraries=['/opt/homebrew/lib/libjpeg.dylib']))
        with self.assertRaisesRegex(ValueError, 'non-system runtime dependency'):
            verify(path, 'macos-arm64')

    def test_build_rpath_is_rejected(self):
        path = self.write(image(rpaths=['/Users/build/vcpkg/installed']))
        with self.assertRaisesRegex(ValueError, 'build rpath'):
            verify(path, 'macos-arm64')

    def test_wrong_architecture_is_rejected(self):
        path = self.write(image(cputype=CPU_TYPES['macos-x64']))
        with self.assertRaisesRegex(ValueError, 'CPU type'):
            verify(path, 'macos-arm64')

    def test_non_macho_input_is_rejected(self):
        path = self.write(b'\x7fELF' + b'\0' * 60)
        with self.assertRaisesRegex(ValueError, 'Mach-O'):
            verify(path, 'macos-arm64')

    def test_inspect_reports_every_dylib_command(self):
        path = self.write(image(libraries=['/usr/lib/libz.dylib', '/usr/lib/libc++.1.dylib']))
        _, _, libraries, _ = inspect(path.read_bytes())
        self.assertEqual(libraries, ['/usr/lib/libz.dylib', '/usr/lib/libc++.1.dylib'])


if __name__ == '__main__':
    unittest.main()

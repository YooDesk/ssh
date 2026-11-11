"""Minimal Mach-O reader: architecture, runtime dependencies and build rpaths.

The packaged macOS binaries must only link Apple's system libraries, exactly like
the Linux archives must not depend on non-system shared objects.
"""

MAGIC_64 = 0xFEEDFACF
CPU_TYPES = {
    'macos-arm64': 0x0100000C,
    'macos-x64': 0x01000007,
}
LC_LOAD_DYLIB = 0xC
LC_LOAD_WEAK_DYLIB = 0x80000018
LC_REEXPORT_DYLIB = 0x8000001F
LC_LOAD_UPWARD_DYLIB = 0x80000023
LC_RPATH = 0x8000001C
DYLIB_COMMANDS = (LC_LOAD_DYLIB, LC_LOAD_WEAK_DYLIB, LC_REEXPORT_DYLIB, LC_LOAD_UPWARD_DYLIB)


def _u32(blob, offset):
    return int.from_bytes(blob[offset:offset + 4], 'little')


def _string(blob, offset):
    end = blob.index(b'\0', offset)
    return blob[offset:end].decode('utf-8', 'replace')


def inspect(blob):
    """Return (cputype, dependencies, rpaths) for a thin 64-bit Mach-O image."""
    if len(blob) < 32 or _u32(blob, 0) != MAGIC_64:
        raise ValueError('Expected a 64-bit little-endian Mach-O image')
    cputype = _u32(blob, 4)
    filetype = _u32(blob, 12)
    commands = _u32(blob, 16)
    offset = 32
    dependencies = []
    rpaths = []
    for _ in range(commands):
        command = _u32(blob, offset)
        size = _u32(blob, offset + 4)
        if size < 8 or offset + size > len(blob):
            raise ValueError('Invalid Mach-O load command')
        if command in DYLIB_COMMANDS:
            # dylib_command: cmd, cmdsize, name offset, timestamp, versions...
            name_offset = _u32(blob, offset + 8)
            dependencies.append(_string(blob, offset + name_offset))
        elif command == LC_RPATH:
            rpaths.append(_string(blob, offset + _u32(blob, offset + 8)))
        offset += size
    return cputype, filetype, dependencies, rpaths


def verify(path, platform):
    blob = path.read_bytes()
    cputype, _, dependencies, rpaths = inspect(blob)
    expected = CPU_TYPES[platform]
    if cputype != expected:
        raise ValueError('{}: CPU type {:#x}, expected {:#x}'.format(path, cputype, expected))
    for name in dependencies:
        if not name.startswith(('/usr/lib/', '/System/Library/')):
            raise ValueError('{}: non-system runtime dependency {}'.format(path, name))
    if rpaths:
        raise ValueError('{}: build rpath in release image: {}'.format(path, ', '.join(rpaths)))
    return {'cputype': cputype, 'libraries': dependencies}

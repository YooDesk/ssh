"""Register only LoongArch ABI 1.0 executables for child-process tests."""
from pathlib import Path
import struct
import subprocess
import sys

entry = Path('/proc/sys/fs/binfmt_misc/yoodesk-la64-oldworld')
if sys.argv[1] == 'remove':
    if entry.exists():
        subprocess.run(['sudo', 'tee', str(entry)], input='-1\n', text=True, check=True, capture_output=True)
    sys.exit(0)

register = entry.with_name('register')
if not register.exists():
    subprocess.run(['sudo', 'mount', '-t', 'binfmt_misc', 'binfmt_misc', str(entry.parent)], check=True)
if entry.exists():
    raise ValueError('Old-world binfmt entry already exists')
magic, mask = bytearray(52), bytearray(52)
magic[:6], mask[:6] = b'\x7fELF\x02\x01', b'\xff' * 6
struct.pack_into('<H', magic, 18, 258)
mask[18:20] = b'\xff\xff'
struct.pack_into('<I', magic, 48, 3)
mask[48:52] = b'\xff' * 4
encode = lambda value: ''.join(f'\\x{byte:02x}' for byte in value)
emulator = Path(sys.argv[1]).resolve()
payload = f':yoodesk-la64-oldworld:M::{encode(magic)}:{encode(mask)}:{emulator}:F\n'
subprocess.run(['sudo', 'tee', str(register)], input=payload, text=True, check=True, capture_output=True)
print('Registered child-process execution for LoongArch ELF flags 0x3')

"""Cargo runner: execute ABI 1.0 tests with their real foreign filesystem."""
import os
from pathlib import Path
import subprocess
import sys

if sys.argv[1:2] == ['--inside']:
    root, workdir, *arguments = sys.argv[2:]
    environment = {
        'PATH': '/usr/bin:/bin', 'HOME': '/tmp', 'TMPDIR': '/tmp',
        'LANG': 'C.UTF-8', 'QEMU_LD_PREFIX': '/',
    }
    for key, value in os.environ.items():
        if key in ('RUST_BACKTRACE', 'RUST_LOG') or key.startswith(('YOODESK_ENGINE_', 'YOODESK_TEST_')):
            environment[key] = value
    os.chroot(root)
    os.chdir(workdir)
    os.execve('/tmp/yoodesk-qemu', ['/tmp/yoodesk-qemu', *arguments], environment)
else:
    root = os.environ['YOODESK_OLDWORLD_ROOT']
    command = ['sudo', sys.executable, str(Path(__file__).resolve()), '--inside', root, os.getcwd(), *sys.argv[1:]]
    sys.exit(subprocess.run(command).returncode)

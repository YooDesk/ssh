"""Install the checksummed Loongnix 20 development/runtime closure for ABI 1.0."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import time
import urllib.request

lock_path = Path(__file__).with_name('oldworld-sysroot.lock.json')
lock = json.loads(lock_path.read_text(encoding='utf-8'))
download_only = '--download-only' in sys.argv
if download_only:
    sys.argv.remove('--download-only')
root = Path(sys.argv[1]).absolute()
cache = Path(sys.argv[2]).absolute()
cache.mkdir(parents=True, exist_ok=True)
marker = root / ('.yoodesk-sysroot-' + hashlib.sha256(lock_path.read_bytes()).hexdigest())
if marker.is_file():
    print('Reusing pinned old-world sysroot')
    sys.exit(0)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fetch(entry):
    path = cache / Path(entry['filename']).name
    if path.is_file() and digest(path) == entry['sha256']:
        return path
    for attempt in range(3):
        try:
            with urllib.request.urlopen(lock['base_url'] + entry['filename'], timeout=60) as response:
                temporary = path.with_suffix(path.suffix + '.partial')
                with temporary.open('wb') as output:
                    while block := response.read(1024 * 1024):
                        output.write(block)
            if digest(temporary) != entry['sha256']:
                raise ValueError('Checksum mismatch: ' + entry['name'])
            temporary.replace(path)
            return path
        except (OSError, ValueError):
            if attempt == 2:
                raise
            time.sleep(2 * (attempt + 1))


mirror_lock = lock_path.with_name('oldworld-sysroot-download.lock.json')
if mirror_lock.is_file() and not download_only:
    mirror = json.loads(mirror_lock.read_text(encoding='utf-8'))
    if digest(lock_path) != mirror['package_lock_sha256']:
        raise ValueError('Released package bundle does not match the sysroot lock')
    bundle_path = cache.parent / 'oldworld-sysroot-inputs.tar'
    if not bundle_path.is_file() or digest(bundle_path) != mirror['sha256']:
        subprocess.run(['curl', '-fL', '--retry', '3', '--connect-timeout', '30',
                        mirror['url'], '-o', str(bundle_path)], check=True)
        if digest(bundle_path) != mirror['sha256']:
            raise ValueError('Released package bundle checksum mismatch')
    expected = {Path(entry['filename']).name for entry in lock['files']}
    with tarfile.open(bundle_path) as bundle:
        entries = bundle.getmembers()
        if {entry.name for entry in entries} != expected or any(not entry.isfile() for entry in entries):
            raise ValueError('Unexpected entries in released package bundle')
        bundle.extractall(cache, filter='data')


with ThreadPoolExecutor(max_workers=6) as pool:
    archives = []
    for index, path in enumerate(pool.map(fetch, lock['files']), 1):
        archives.append(path)
        if index % 40 == 0:
            print('Verified', index, 'old-world packages', flush=True)
if download_only:
    print('All', len(archives), 'package downloads verified', flush=True)
    sys.exit(0)
for archive in archives:
    subprocess.run(['dpkg-deb', '-x', str(archive), str(root)], check=True)

# Keep every absolute target link within the foreign filesystem on the host.
for path in root.rglob('*'):
    if path.is_symlink():
        target = os.readlink(path)
        if target.startswith('/'):
            relative = os.path.relpath(root / target.lstrip('/'), path.parent)
            path.unlink()
            path.symlink_to(relative)
marker.touch()
print('Installed', len(archives), 'pinned Loongnix 20 packages')

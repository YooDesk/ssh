"""Apply reviewed build-only files while retaining the exact runtime source SHA."""
from pathlib import Path
import re
import subprocess
import sys

# Changes outside this explicit list invalidate reuse of native artifacts.
# Runtime sources, dependency manifests, locks and engine patches are excluded.
ADAPTER_PATHS = {
    'ci/build_adapters.py', 'ci/build_matrix.py', 'ci/linux-build.sh',
    'ci/audit_engine_sysroot.py', 'ci/native_sdk.py', 'ci/native-sdk.lock.json', 'ci/package.py',
    'ci/windows_crt.py', 'ci/test_windows_crt.py', 'ci/test_oldworld.py', 'ci/publish_verified_plugins.py',
    'ci/oldworld-binfmt.py', 'ci/oldworld-build.sh', 'ci/oldworld-cargo.sh',
    'ci/oldworld-codecs.sh', 'ci/oldworld-qemu.sh', 'ci/oldworld-runner.py',
    'ci/oldworld-rust.lock.json', 'ci/oldworld-sysroot-download.lock.json',
    'ci/oldworld-sysroot.lock.json', 'ci/oldworld-sysroot.py',
    'ci/oldworld-toolchain.sh', 'ci/oldworld-tools.lock.json',
    'scripts/build_engine.py', 'scripts/prepare_vcpkg_tool.py',
    'scripts/prepare_vcpkg_downloads.py',
}
REVIEW_ONLY_PATHS = {
    '.github/workflows/build.yml', '.github/workflows/audit-oldworld.yml', '.github/workflows/publish-recovery.yml',
    'README.md', 'PROGRESS.md', 'MIGRATION.md',
}


def review_changes(source, build_tools='HEAD'):
    paths = subprocess.check_output(
        ['git', 'diff', '--name-only', source, build_tools], text=True).splitlines()
    rejected = [path for path in paths if path not in ADAPTER_PATHS | REVIEW_ONLY_PATHS
                and not path.startswith('docs/')]
    if rejected:
        raise ValueError('Artifact reuse is restricted to build-tool/documentation changes: ' + ', '.join(rejected))
    return sorted(set(paths) & ADAPTER_PATHS)


if __name__ == '__main__':
    source, build_tools = sys.argv[1:]
    if not all(re.fullmatch('[0-9a-f]{40}', value) for value in (source, build_tools)):
        raise ValueError('Adapter revisions must be exact commit SHAs')
    checkout = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    if checkout != source:
        raise ValueError('Build checkout does not match the pinned source SHA')
    paths = review_changes(source, build_tools)
    for name in paths:
        payload = subprocess.check_output(['git', 'show', build_tools + ':' + name])
        path = Path(name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
    print('Applied', len(paths), 'reviewed CI adapters to runtime source', source)

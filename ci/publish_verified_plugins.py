"""Upload verified plugin archives to an existing release without changing it."""
import hashlib
import json
import os
from pathlib import Path
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

source = os.environ['SOURCE_SHA']
release_id = os.environ['RELEASE_ID']
repository = os.environ['GITHUB_REPOSITORY']
kind = repository.split('/')[-1]
assert kind in ('desk', 'ssh', 'rdp', 'vnc')
assert re.fullmatch('[0-9a-f]{40}', source) and release_id.isdecimal()
base = 'https://api.github.com/repos/' + repository
headers = {'Authorization': 'Bearer ' + os.environ['GH_TOKEN'],
           'Accept': 'application/vnd.github+json', 'User-Agent': 'yoodesk-release'}


def api(url, data=None, content='application/json'):
    request = urllib.request.Request(url, data=data, headers={**headers, 'Content-Type': content})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=180) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code < 500 or attempt == 2:
                raise
            time.sleep(2 * (attempt + 1))


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


release = api(base + '/releases/' + release_id)
assert release['target_commitish'] == source
tag = api(base + '/git/ref/tags/' + urllib.parse.quote(release['tag_name'], safe=''))
assert tag['object']['sha'] == source
platforms = {'windows-x86', 'windows-x64', 'windows-arm64', 'linux-x64', 'linux-arm64',
             'linux-loong64', 'linux-loong64-oldworld', 'linux-mips64el', 'macos-arm64', 'macos-x64'}
archives = sorted(Path('dist').glob('*.zip'))
assert len(archives) == len(platforms)
expected = set()
for path in archives:
    checksum_path = Path(str(path) + '.sha256')
    checksum, filename = checksum_path.read_text().split()
    assert filename == path.name and checksum == digest(path)
    with zipfile.ZipFile(path) as archive:
        metadata = json.loads(archive.read('plugin.json'))
        platform = metadata['platform']
        platforms.remove(platform)
        assert path.name == f'yoodesk-{kind}-{platform}.zip'
        assert metadata['id'] == kind and metadata['abi_version'] == 1
        assert metadata['source_commit'] == source
        assert hashlib.sha256(archive.read(metadata['library'])).hexdigest() == metadata['sha256']
    expected.update((path.name, checksum_path.name))
assert not platforms
existing = {asset['name']: asset for asset in release['assets']}
assert set(existing).issubset(expected)
for name in sorted(expected):
    path = Path('dist') / name
    checksum = digest(path)
    if name in existing:
        assert existing[name]['digest'] == 'sha256:' + checksum
        continue
    url = 'https://uploads.github.com/repos/' + repository + '/releases/' + release_id + '/assets?name=' + urllib.parse.quote(name)
    uploaded = api(url, path.read_bytes(), 'application/octet-stream')
    assert uploaded['size'] == path.stat().st_size and uploaded['digest'] == 'sha256:' + checksum
    print('Verified and uploaded', name, flush=True)
uploaded = api(base + '/releases/' + release_id)
assert {asset['name'] for asset in uploaded['assets']} == expected
print('All ten platforms uploaded; release state preserved:', 'draft' if uploaded['draft'] else 'published')

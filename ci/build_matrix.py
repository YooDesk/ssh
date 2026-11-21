"""Select native targets; only the complete matrix is eligible for a release."""
import json
import os
import subprocess
import urllib.request
import re
import sys
from build_adapters import review_changes

targets = [
    dict(platform='windows-x86', os='windows-2022', target='i686-pc-windows-msvc', triplet='x86-windows-static', msvc='x64_x86'),
    dict(platform='windows-x64', os='windows-2022', target='x86_64-pc-windows-msvc', triplet='x64-windows-static', msvc='x64'),
    dict(platform='windows-arm64', os='windows-11-arm', target='aarch64-pc-windows-msvc', triplet='arm64-windows-static', msvc='x64_arm64'),
    dict(platform='linux-x64', os='ubuntu-22.04', target='x86_64-unknown-linux-gnu', triplet='x64-linux'),
    dict(platform='linux-arm64', os='ubuntu-22.04-arm', target='aarch64-unknown-linux-gnu', triplet='arm64-linux'),
    dict(platform='linux-loong64', os='ubuntu-22.04', target='loongarch64-unknown-linux-gnu', triplet='loongarch64-linux', cross=True),
    dict(platform='linux-loong64-oldworld', os='ubuntu-24.04', target='loongarch64-unknown-linux-gnu', cross=True, oldworld=True),
    dict(platform='linux-mips64el', os='ubuntu-22.04', target='mips64el-unknown-linux-gnuabi64', triplet='mips64el-linux', cross=True),
    # macOS 两个架构都在对应的原生 runner 上编译，避免交叉编译原生依赖。
    dict(platform='macos-arm64', os='macos-15', target='aarch64-apple-darwin', triplet='arm64-osx', macos=True),
    dict(platform='macos-x64', os='macos-15-intel', target='x86_64-apple-darwin', triplet='x64-osx', macos=True),
]
platform = os.environ.get('PLATFORM', 'all')
selected = [target for target in targets if platform in ['all', target['platform']]]
if not selected:
    raise ValueError(f'Unknown platform: {platform}')
source = os.environ.get('SOURCE_REF') or subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
if not re.fullmatch('[0-9a-f]{40}', source):
    raise ValueError('Source ref must be a full commit SHA')
reuse = os.environ.get('REUSE_RUN_ID', '')
if reuse:
    if platform != 'all' or not reuse.isdecimal():
        raise ValueError('Reuse requires the complete release matrix and a numeric run ID')
    subprocess.run(['git','fetch','--depth','1','origin',source],check=True)
    review_changes(source)
    base = 'https://api.github.com/repos/'+os.environ['GITHUB_REPOSITORY']
    def api(path):
        request = urllib.request.Request(base+path,headers={'Authorization':'Bearer '+os.environ['GH_TOKEN'],'Accept':'application/vnd.github+json','User-Agent':'yoodesk-matrix'})
        with urllib.request.urlopen(request,timeout=45) as response:
            return json.load(response)
    run = api('/actions/runs/'+reuse)
    if run['head_sha'] != source or run['status'] != 'completed':
        raise ValueError('Reused run must have finished and match the exact pinned source commit')
    artifacts = api('/actions/runs/'+reuse+'/artifacts?per_page=100')
    if artifacts['total_count'] > 100:
        raise ValueError('Unexpected artifact count')
    reusable = {item['name'].removeprefix('native-') for item in artifacts['artifacts'] if item['name'].startswith('native-') and not item['expired'] and item['size_in_bytes'] > 0}
    selected = [target for target in selected if target['platform'] not in reusable]
    if not selected:
        raise ValueError('All platforms already exist; use their completed release')
    print('Reusing completed platforms: '+','.join(sorted(reusable)),file=sys.stderr)
print('source_sha='+source)
print('matrix=' + json.dumps({'include': selected}, separators=(',', ':')))

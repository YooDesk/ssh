"""Create and validate release archives; plugin binaries are never linked into the host."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import tempfile
import zipfile
from windows_crt import verify as verify_windows_dependencies

PUBLIC_EXPORTS={'yd_plugin_v1'}


def prepare_plugin_exports(library):
    subprocess.run(['strip','--strip-unneeded',str(library)],check=True)
    symbols=subprocess.check_output(['nm','-D','--defined-only','--format=posix',str(library)],text=True)
    exported={line.split()[0].split('@',1)[0] for line in symbols.splitlines() if line.strip()}
    if exported!=PUBLIC_EXPORTS:raise ValueError(f'{library}: unexpected exported symbols: {sorted(exported)}')

PLATFORMS = {
    "windows-x64": ("x86_64-pc-windows-msvc", 0x8664),
    "windows-arm64": ("aarch64-pc-windows-msvc", 0xAA64),
    "linux-x64": ("x86_64-unknown-linux-gnu", 62),
    "linux-arm64": ("aarch64-unknown-linux-gnu", 183),
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify_binary(path, platform):
    data = path.read_bytes()
    expected = PLATFORMS[platform][1]
    if platform.startswith("windows"):
        if data[:2] != b"MZ": raise ValueError(f"Not a PE binary: {path}")
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        if data[offset:offset+4] != b"PE\0\0": raise ValueError("Invalid PE header")
        machine = struct.unpack_from("<H", data, offset+4)[0]
        verify_windows_dependencies(path)
    else:
        if data[:4] != b"\x7fELF" or data[4:6] != b"\x02\x01": raise ValueError(f"Not a 64-bit little-endian ELF: {path}")
        machine = struct.unpack_from("<H", data, 18)[0]
        output = subprocess.check_output(["readelf", "--version-info", str(path)], text=True)
        versions = [tuple(map(int, value.split("."))) for value in re.findall(r"GLIBC_(\d+\.\d+)", output)]
        if versions and max(versions) > (2, 27): raise ValueError(f"{path}: requires GLIBC {max(versions)}, above Ubuntu 18.04")
    if machine != expected: raise ValueError(f"{path}: machine {machine:#x}, expected {expected:#x}")


def archive(stage, output):
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
        for path in sorted(stage.rglob("*")):
            if path.is_file(): bundle.write(path, path.relative_to(stage).as_posix())
    output.with_suffix(output.suffix + ".sha256").write_text(f"{digest(output)}  {output.name}\n", encoding="utf-8")


def extract(bundle, destination):
    with zipfile.ZipFile(bundle) as archive_file:
        for entry in archive_file.infolist():
            target = (destination / entry.filename).resolve()
            if not target.is_relative_to(destination.resolve()): raise ValueError("Archive path escapes bundle")
            if (entry.external_attr >> 16) & 0o170000 == 0o120000: raise ValueError("Archive symlinks are not accepted")
        archive_file.extractall(destination)


def plugin(root, platform, kind, output):
    target = PLATFORMS[platform][0]
    stem=kind.replace('-','_')
    filename = f"yoodesk_{stem}.dll" if platform.startswith("windows") else f"libyoodesk_{stem}.so"
    library = root / "target" / target / "release" / filename
    verify_binary(library, platform)
    if platform.startswith('linux'):prepare_plugin_exports(library)
    with tempfile.TemporaryDirectory() as temporary:
        stage = Path(temporary)
        shutil.copy2(library, stage / filename)
        browser = root / "browser"
        if browser.is_dir(): shutil.copytree(browser, stage / "assets" / kind)
        if kind == "rdp":
            wasm = browser / "ironrdp" if (browser / "ironrdp").is_dir() else root / "assets/ironrdp"
            shutil.copytree(wasm, stage / "assets/rdp/ironrdp", dirs_exist_ok=True)
        if kind == "desk" and platform.startswith("linux"):
            for name in ["libdrmtap.so.0", "libdrmtap.manifest.json", "LICENSE.libdrmtap"]:
                source = root / "target/release" / name
                if not source.is_file(): raise ValueError(f"Missing desktop runtime attachment: {source}")
                shutil.copy2(source, stage / name)
            verify_binary(stage / "libdrmtap.so.0", platform)
        for name in ["LICENSE", "SOURCE.json"]:
            if (root / name).is_file(): shutil.copy2(root / name, stage / name)
        metadata = {"schema_version":1, "abi_version":1, "id":kind, "platform":platform, "library":filename, "sha256":digest(library), "source_commit":os.environ.get("GITHUB_SHA", "local")}
        (stage / "plugin.json").write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
        archive(stage, output / f"yoodesk-{kind}-{platform}.zip")


def host(root, platform, plugins, output):
    target = PLATFORMS[platform][0]
    filename = "web-service.exe" if platform.startswith("windows") else "web-service"
    binary = root / "target" / target / "release" / filename
    verify_binary(binary, platform)
    lock = json.loads((plugins / "plugins.lock.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory() as temporary:
        stage = Path(temporary)
        shutil.copy2(binary, stage / filename)
        (stage / "plugins").mkdir()
        entries = []
        for kind,record in lock['repositories'].items():
            asset = record["assets"][platform]
            package = plugins / asset["name"]
            if digest(package) != asset["sha256"]: raise ValueError(f"{kind}: archive checksum mismatch")
            with tempfile.TemporaryDirectory() as unpack:
                unpack = Path(unpack)
                extract(package, unpack)
                meta = json.loads((unpack / "plugin.json").read_text(encoding="utf-8"))
                if (meta["id"], meta["platform"], meta["abi_version"]) != (kind, platform, 1): raise ValueError("Plugin release metadata mismatch")
                stem=kind.replace('-','_')
                expected_library=f"yoodesk_{stem}.dll" if platform.startswith("windows") else f"libyoodesk_{stem}.so"
                if meta['library']!=expected_library: raise ValueError("Plugin release library name mismatch")
                library = unpack / meta["library"]
                if digest(library) != meta["sha256"]: raise ValueError("Plugin library checksum mismatch")
                verify_binary(library, platform)
                if platform.startswith('linux'):prepare_plugin_exports(library)
                shutil.copy2(library, stage / "plugins" / library.name)
                entry={"id":kind,"file":library.name,"sha256":digest(library),"target_scope":record.get('target_scope','remote')}
                if 'options' in record:entry['options']=record['options']
                entries.append(entry)
                if (unpack / "assets").exists(): shutil.copytree(unpack / "assets",stage / "assets",dirs_exist_ok=True)
                if kind == "desk" and platform.startswith("linux"):
                    for name in ["libdrmtap.so.0", "libdrmtap.manifest.json", "LICENSE.libdrmtap"]: shutil.copy2(unpack / name,stage / name)
                for name in ["LICENSE","SOURCE.json"]:
                    if (unpack/name).is_file():
                        directory=stage/"licenses"/kind
                        directory.mkdir(parents=True,exist_ok=True)
                        shutil.copy2(unpack/name,directory/name)
        (stage/"plugins/plugins.json").write_text(json.dumps({"abi_version":1,"plugins":entries},indent=2)+"\n",encoding="utf-8")
        shutil.copy2(plugins/"plugins.lock.json",stage/"plugins.lock.json")
        for name in ["README.md","LICENSE","SOURCE.json"]: shutil.copy2(root/name,stage/name)
        if (root/"configs").exists(): shutil.copytree(root/"configs",stage/"configs")
        (stage/"manifest.json").write_text(json.dumps({"product_id":"yoodesk","version":os.environ.get("RELEASE_VERSION","0.1.0"),"platform":platform,"source_commit":os.environ.get("GITHUB_SHA","local")},indent=2)+"\n",encoding="utf-8")
        archive(stage,output/f"yoodesk-webservice-{platform}.zip")


if __name__ == "__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind")
    parser.add_argument("--platform",choices=PLATFORMS,required=True)
    parser.add_argument("--plugins",type=Path,default=Path("artifacts/plugins"))
    parser.add_argument("--output",type=Path,default=Path("dist"))
    args=parser.parse_args()
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,31}',args.kind):parser.error('Invalid plugin id')
    root=Path(__file__).resolve().parents[1]
    if args.kind=="webservice":host(root,args.platform,args.plugins.resolve(),args.output.resolve())
    else:plugin(root,args.platform,args.kind,args.output.resolve())

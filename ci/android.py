"""Build and verify the independently released Android ARM64 component."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGET = "aarch64-linux-android"
API = 24
NDK = "26.1.10909125"


def run(*args, **kwargs):
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def environment():
    sdk = Path(os.environ.get("ANDROID_HOME") or os.environ["ANDROID_SDK_ROOT"])
    ndk = sdk / "ndk" / NDK
    tools = ndk / "toolchains/llvm/prebuilt/linux-x86_64/bin"
    clang = tools / "clang"
    link = ROOT / "artifacts/android/cxx-link"
    link.mkdir(parents=True, exist_ok=True)
    for name in ("libc++_static.a", "libc++abi.a"):
        shutil.copy2(tools.parent / "sysroot/usr/lib" / TARGET / name, link / name)
    env = dict(os.environ, ANDROID_NDK_HOME=str(ndk), ANDROID_NDK_ROOT=str(ndk), ANDROID_NDK=str(ndk), CARGO_NET_GIT_FETCH_WITH_CLI="true")
    flags = env.get("CARGO_ENCODED_RUSTFLAGS", "").split("\x1f") if env.get("CARGO_ENCODED_RUSTFLAGS") else []
    flags += ["-C", f"link-arg=--target={TARGET}{API}", "-C", "link-arg=-Wl,-z,max-page-size=16384", "-L", f"native={link}"]
    env.update(CC_aarch64_linux_android=str(clang), CXX_aarch64_linux_android=str(tools / "clang++"),
               AR_aarch64_linux_android=str(tools / "llvm-ar"),
               CFLAGS_aarch64_linux_android=f"--target={TARGET}{API}",
               CXXFLAGS_aarch64_linux_android=f"--target={TARGET}{API}",
               CXXSTDLIB_aarch64_linux_android="c++_static", CXXSTDLIB_STATIC_aarch64_linux_android="0",
               CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=str(clang),
               CARGO_ENCODED_RUSTFLAGS="\x1f".join(flags))
    return env, tools


def verify(path, tools):
    output = subprocess.check_output([str(tools / "llvm-readelf"), "-h", "-lW", "-d", str(path)], text=True)
    if "AArch64" not in output or "[libc.so]" not in output:
        raise ValueError(f"Expected an Android ARM64 binary using shared Bionic: {path}")
    loads = [line.split() for line in output.splitlines() if line.strip().startswith("LOAD ")]
    if not loads or any(int(line[-1], 16) < 16384 for line in loads):
        raise ValueError(f"Native library is not aligned for 16 KiB pages: {path}")
    allowed = {"libc.so", "libm.so", "libdl.so", "liblog.so", "libandroid.so", "libz.so"}
    import re
    needed = set(re.findall(r"Shared library: \[([^\]]+)\]", output))
    if needed - allowed:
        raise ValueError(f"Unexpected Android shared dependencies: {needed - allowed}")


def main():
    component = sys.argv[1]
    if component not in {"connector", "desk", "webservice", "ssh", "rdp", "vnc"}:
        raise ValueError("Unknown component")
    env, tools = environment()
    target = ROOT / "artifacts/android/target"
    stage = ROOT / "artifacts/android/stage"
    stage.mkdir(parents=True, exist_ok=True)
    if component == "desk":
        run(sys.executable, "scripts/prepare_openh264.py")
    command = ["cargo", "build", "--locked", "--release", "--target", TARGET, "--target-dir", target]
    command += ["--bin", "connector-http"] if component == "connector" else ["--no-default-features", "--lib"]
    run(*command, env=env)
    binary = {"connector": "connector-http", "desk": "libyoodesk_desk.so", "webservice": "libweb_service.so", "ssh": "libyoodesk_ssh.so", "rdp": "libyoodesk_rdp.so", "vnc": "libyoodesk_vnc.so"}[component]
    shutil.copy2(target / TARGET / "release" / binary, stage / binary)
    if component == "desk":
        sys.path.insert(0, str(ROOT / "scripts"))
        from android_capture import build_rustdesk_capture
        from prepare_sources import checkout
        tree = target.parent / "rustdesk-src"
        checkout("rustdesk", tree)
        baseline = json.loads((tree / "vcpkg.json").read_text())["builtin-baseline"]
        vcpkg = target.parent / "vcpkg"
        if not (vcpkg / ".git").exists():
            run("git", "init", vcpkg)
            run("git", "-C", vcpkg, "remote", "add", "origin", "https://github.com/microsoft/vcpkg.git")
            run("git", "-C", vcpkg, "fetch", "--depth", "1", "origin", baseline)
            run("git", "-C", vcpkg, "checkout", "--detach", "FETCH_HEAD")
        actual = subprocess.check_output(["git", "-C", str(vcpkg), "rev-parse", "HEAD"], text=True).strip()
        if actual != baseline:
            raise ValueError("vcpkg baseline does not match pinned RustDesk")
        run("bash", vcpkg / "bootstrap-vcpkg.sh", "-disableMetrics")
        env["VCPKG_ROOT"] = str(vcpkg)
        capture = build_rustdesk_capture(tools / "clang", target, env, vcpkg / "vcpkg")
        shutil.copy2(capture, stage / capture.name)
        for notice in (target.parent / "vcpkg-installed/arm64-android/share").glob("*/copyright"):
            licenses = stage / "licenses"
            licenses.mkdir(exist_ok=True)
            shutil.copy2(notice, licenses / (notice.parent.name + "-copyright"))
        shutil.copytree(ROOT / "browser_static", stage / "browser", dirs_exist_ok=True)
        # Preserve upstream license attribution in the independently released archive.
        for name in ("LICENSE", "LICENCE"):
            if (tree / name).is_file():
                shutil.copy2(tree / name, stage / "RustDesk-LICENSE")
    if component in {"ssh", "rdp", "vnc"}:
        shutil.copytree(ROOT / "browser", stage / "browser", dirs_exist_ok=True)
    if component == "connector":
        admin = ROOT / "src/bin/webadmin"
        shutil.copytree(admin, stage / "webadmin", dirs_exist_ok=True)
    copied = 0
    for notice in ROOT.glob("LICENSE*"):
        if notice.is_file():
            shutil.copy2(notice, stage / notice.name)
            copied += 1
    if not copied:
        raise ValueError("Component license is missing")
    for vendor in (ROOT / "vendor").glob("*"):
        if vendor.is_dir():
            for notice in vendor.glob("LICENSE*"):
                destination = stage / "licenses" / vendor.name
                destination.mkdir(parents=True, exist_ok=True)
                shutil.copy2(notice, destination / notice.name)
            for name in ("SOURCE.json", "PATCHES.md"):
                if (vendor / name).is_file():
                    destination = stage / "licenses" / vendor.name
                    destination.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(vendor / name, destination / name)
    for name in ("SOURCE.json", "dependencies.lock.json"):
        if (ROOT / name).is_file():
            shutil.copy2(ROOT / name, stage / name)
    for path in stage.iterdir():
        if path.name == binary or path.suffix == ".so":
            verify(path, tools)
    manifest = {"schema_version": 1, "component": component, "platform": "android-arm64",
                "ndk_version": NDK, "android_api": API, "page_alignment": 16384,
                "commit": os.environ.get("GITHUB_SHA") or subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "files": {str(path.relative_to(stage)).replace("\\", "/"): hashlib.sha256(path.read_bytes()).hexdigest()
                          for path in stage.rglob("*") if path.is_file() and path.name != "android-component.json"}}
    (stage / "android-component.json").write_text(json.dumps(manifest, indent=2) + "\n")
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    archive = dist / f"yoodesk-{component}-android-arm64.zip"
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as bundle:
        for path in sorted(stage.rglob("*")):
            if path.is_file():
                bundle.write(path, path.relative_to(stage))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
    print(archive)


if __name__ == "__main__":
    main()

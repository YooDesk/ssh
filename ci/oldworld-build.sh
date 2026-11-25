#!/usr/bin/env bash
# Build an ABI 1.0 component with released, checksummed host tools and sysroot.
# QEMU release policy: no full suites or throughput gates.
set -euo pipefail
kind="${1:?component required}"
workspace="$PWD"
platform=linux-loong64-oldworld
target=loongarch64-unknown-linux-gnu
tools=/opt/yoodesk-oldworld
# Match the existing disposable GitHub runner cleanup before engine builds.
if [[ "${GITHUB_ACTIONS:-}" == true ]]; then
    sudo rm -rf /opt/ghc /usr/local/lib/android /usr/share/dotnet
fi
sudo mkdir -p "$tools"
sudo chown "$(id -u):$(id -g)" "$tools"
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    build-essential pkg-config python3-dev python3-pip python3-venv \
    cmake ninja-build nasm autoconf automake libtool libssl-dev libclang-dev
python3 -m venv "$tools/python"
"$tools/python/bin/pip" install 'meson==1.9.0' 'cmake==3.31.6'
export PATH="$tools/python/bin:$PATH"
read -r sdk_url sdk_sha < <(python3 - <<'PY'
import json
record = json.load(open('ci/oldworld-tools.lock.json'))
print(record['url'], record['sha256'])
PY
)
mkdir -p .ci-cache
archive="$workspace/.ci-cache/oldworld-toolchain.tar.xz"
if [[ ! -f "$archive" ]] || ! echo "$sdk_sha  $archive" | sha256sum -c - >/dev/null 2>&1; then
    curl -fL --retry 3 --connect-timeout 30 "$sdk_url" -o "$archive"
    echo "$sdk_sha  $archive" | sha256sum -c -
fi
tar -xJf "$archive" -C "$tools"
bash ci/oldworld-cargo.sh "$tools" "$workspace/.ci-cache/oldworld-upstream-cargo.tar.xz"
export YOODESK_OLDWORLD_TOOLS="$tools"
export YOODESK_OLDWORLD_ROOT="$tools/toolchain/sysroot"
root="$YOODESK_OLDWORLD_ROOT"
if [[ "$kind" == desk ]]; then
    sdk_record=$(python3 ci/native_sdk.py location "$platform")
    if [[ -n "$sdk_record" ]]; then
        read -r native_url native_sha <<< "$sdk_record"
        native_archive="$workspace/.ci-cache/oldworld-native-sdk.tar.xz"
        if [[ ! -f "$native_archive" ]] || ! echo "$native_sha  $native_archive" | sha256sum -c - >/dev/null 2>&1; then
            curl -fL --retry 3 --connect-timeout 30 "$native_url" -o "$native_archive"
            echo "$native_sha  $native_archive" | sha256sum -c -
        fi
        tar -xJf "$native_archive" -C "$root"
        python3 ci/native_sdk.py verify "$platform" "$root"
        export OPENH264_LIB_DIR="$root/yoodesk-native/openh264"
        export MOZJPEG_LIB_DIR="$root/yoodesk-native/mozjpeg"
    fi
fi
python3 ci/oldworld-sysroot.py "$root" "$workspace/.ci-cache/oldworld-debs"
export PATH="$tools/upstream-cargo/bin:$tools/rust/bin:$tools/toolchain/bin:$tools/toolchain/llvm/bin:$PATH"
export RUSTC="$tools/rust/bin/rustc" RUSTDOC="$tools/rust/bin/rustdoc"
export CARGO_HOME="$workspace/.ci-cache/cargo-oldworld"
export CARGO_BUILD_TARGET="$target"
export CARGO_NET_GIT_FETCH_WITH_CLI=true
export CC_loongarch64_unknown_linux_gnu="$tools/toolchain/bin/clang"
export CXX_loongarch64_unknown_linux_gnu="$tools/toolchain/bin/clang++"
export AR_loongarch64_unknown_linux_gnu="$tools/toolchain/llvm/bin/llvm-ar"
export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC_loongarch64_unknown_linux_gnu"
export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C code-model=large'
export CFLAGS_loongarch64_unknown_linux_gnu='-mcmodel=large'
export CXXFLAGS_loongarch64_unknown_linux_gnu='-mcmodel=large'
export CC="$CC_loongarch64_unknown_linux_gnu" CXX="$CXX_loongarch64_unknown_linux_gnu"
export YOODESK_BINDGEN_CLANG="$CC_loongarch64_unknown_linux_gnu"
export HOST_CC=/usr/bin/gcc HOST_CXX=/usr/bin/g++
export CC_x86_64_unknown_linux_gnu=/usr/bin/gcc CXX_x86_64_unknown_linux_gnu=/usr/bin/g++
export AR_x86_64_unknown_linux_gnu=/usr/bin/ar
export HOST_PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
export HOST_PKG_CONFIG_LIBDIR="$HOST_PKG_CONFIG_PATH" HOST_PKG_CONFIG_SYSROOT_DIR=/
export PKG_CONFIG=/usr/bin/pkg-config PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$root"
export PKG_CONFIG_LIBDIR="$root/usr/lib/loongarch64-linux-gnu/pkgconfig:$root/usr/lib64/pkgconfig:$root/usr/lib/pkgconfig:$root/usr/share/pkgconfig"
export PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export LIBCLANG_PATH="$tools/toolchain/llvm/lib"
export LD_LIBRARY_PATH="$tools/toolchain/host-libs/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export BINDGEN_EXTRA_CLANG_ARGS="--target=loongarch64-linux-gnuabi64 --sysroot=$root -mabi=lp64"
rustc -vV

# /opt is outside the source tree, so these mounts cannot recurse into the root.
sudo mkdir -p "$root$workspace" "$root/proc" "$root/dev" "$root/tmp"
sudo chmod 1777 "$root/tmp"
sudo cp "$tools/qemu-oldworld/bin/qemu-loongarch64" "$root/tmp/yoodesk-qemu"
mounts=()
cleanup() {
    python3 "$workspace/ci/oldworld-binfmt.py" remove || true
    for ((i=${#mounts[@]}-1; i>=0; i--)); do
        sudo umount "${mounts[i]}" || true
    done
}
trap cleanup EXIT
export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_RUNNER="python3 $workspace/ci/oldworld-runner.py"

if [[ "$kind" == desk ]]; then
    bash ci/oldworld-codecs.sh
    export AOM_STATIC=1 LIBYUV_STATIC=1
    export YOODESK_STATIC_AOM_DIR="$root/usr/lib/loongarch64-linux-gnu"
    export YOODESK_STATIC_YUV_DIR="$YOODESK_STATIC_AOM_DIR"
    # Loongnix 20's libdrm 2.4.97 lacks drmModeFB2; retain X11/Portal capture.
    export YOODESK_SKIP_DRMTAP=1
    export SODIUM_LIB_DIR="$root/usr/lib/loongarch64-linux-gnu"
    export YOODESK_NATIVE_SDK_ROOT="$root"
    python3 ci/audit_engine_sysroot.py --root "$root" --platform "$platform" \
        --check-headers --report "artifacts/sysroot-audit-$platform.json"
    if [[ "${YOODESK_SYSROOT_AUDIT_ONLY:-0}" == 1 ]]; then
        mkdir -p dist
        cp "artifacts/sysroot-audit-$platform.json" dist/
        exit 0
    fi
    python3 scripts/prepare_sources.py
    python3 scripts/prepare_linux_native.py
    python3 scripts/patch_hwcodec.py
    python3 scripts/prepare_nix.py
    cp desk-engine/Cargo.lock artifacts/oldworld-engine-lock.before
    if ! python3 scripts/build_engine.py 2>&1 | tee artifacts/oldworld-engine-build.log; then
        # Diagnose vendor Cargo's required lock change, without accepting it as
        # a release build. A reviewed, committed lock is still mandatory.
        if grep -q 'cannot update the lock file' artifacts/oldworld-engine-build.log; then
            (cd desk-engine && cargo fetch) || true
            cp desk-engine/Cargo.lock artifacts/oldworld-engine-lock.after
            git diff -- desk-engine/Cargo.lock > artifacts/oldworld-engine-lock.diff
            cat artifacts/oldworld-engine-lock.diff
        fi
        exit 1
    fi
    export YOODESK_DESK_ENGINE_LIB_DIR="$workspace/desk-engine/target/$target/release"
    export YOODESK_DESK_ENGINE=1
    mkdir -p target/release
    cp "$root/usr/share/doc/yoodesk-codecs/"LICENSE.static-* target/release/
fi
cargo build --locked --release --lib
python3 ci/oldworld-binfmt.py "$tools/qemu-oldworld/bin/qemu-loongarch64"
for entry in "$workspace" /proc /dev; do
    sudo mount --bind "$entry" "$root$entry"
    mounts+=("$root$entry")
done
cleanup
for entry in "${mounts[@]}"; do
    if mountpoint -q "$entry"; then
        echo "Cannot package a sysroot with a remaining test mount: $entry" >&2
        exit 1
    fi
done
mounts=()
python3 ci/package.py "$kind" --platform "$platform"
if [[ "$kind" == desk ]]; then
    python3 ci/native_sdk.py package "$platform"
fi

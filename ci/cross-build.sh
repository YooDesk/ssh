#!/usr/bin/env bash
# Cross-compile a YooDesk Rust component for loongarch64 or mips64el inside
# the Ubuntu 18.04 container.
#
# Usage: ci/cross-build.sh <platform> <target> <kind>
#   platform  release platform id (linux-loong64 | linux-mips64el)
#   target    Rust target triple
#   kind      component kind passed to ci/package.py (connector passes none;
#             plugins pass their plugin id, webservice passes "webservice")
#
# Environment:
#   RUST_VERSION  Rust toolchain version (default 1.99.0)
#
# The script sets up the pinned cross toolchain (ci/cross-tools.sh), installs
# the Rust target (official std for loongarch64, rust-src + build-std for
# mips64el), builds and unit-tests under QEMU user mode, then packages the
# release archive with ci/package.py.
set -euo pipefail
platform="$1"
target="$2"
kind="${3:-}"

export DEBIAN_FRONTEND=noninteractive
# archive.ubuntu.com occasionally refuses connections from runners; retry
# the index update before declaring the build environment unusable.
for attempt in 1 2 3; do
    apt-get update && break
    if [[ "$attempt" == 3 ]]; then
        echo "apt-get update failed after 3 attempts" >&2
        exit 1
    fi
    sleep $((attempt * 15))
done
for attempt in 1 2 3; do
    apt-get install -y --no-install-recommends build-essential gcc-8 g++-8 ca-certificates curl git xz-utils pkg-config python3 binutils file dpkg zlib1g-dev libffi-dev libssl-dev libbz2-dev liblzma-dev libreadline-dev libsqlite3-dev && break
    if [[ "$attempt" == 3 ]]; then
        echo "dependency install failed after 3 attempts" >&2
        exit 1
    fi
    apt-get update && sleep $((attempt * 15))
done

tools=/opt/yoodesk-tools
mkdir -p "$tools"
if [[ ! -x "$tools/bin/python3.13" ]]; then
    curl -fL --retry 3 https://www.python.org/ftp/python/3.13.5/Python-3.13.5.tgz -o /tmp/python.tgz
    tar -xf /tmp/python.tgz -C /tmp
    pushd /tmp/Python-3.13.5
    ./configure --prefix="$tools" --with-ensurepip=install
    make -j3
    make install
    popd
fi
export PATH="$tools/bin:$PATH"

case "$platform" in
linux-loong64) kind_tools=loongarch64 ;;
linux-mips64el) kind_tools=mips64el ;;
*)
    echo "Unsupported cross platform: $platform" >&2
    exit 1
    ;;
esac

bash "$(dirname "$0")/cross-tools.sh" "$kind_tools" "$tools"

if [[ ! -x "$tools/bin/cargo" ]]; then
    curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu.tar.xz" -o /tmp/rust.tar.xz
    tar -xf /tmp/rust.tar.xz -C /tmp
    "/tmp/rust-${RUST_VERSION:-1.99.0}-x86_64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
fi
export PATH="$tools/bin:$PATH"
export CARGO_HOME=/workspace/.ci-cache/cargo
export CARGO_BUILD_TARGET="$target"

cd /workspace
git config --global --add safe.directory /workspace

case "$platform" in
linux-loong64)
    # Official Tier 2 std; install the separate target package.
    if [[ ! -d "$tools/lib/rustlib/$target" ]]; then
        curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu.tar.xz" -o /tmp/rust-std.tar.xz
        tar -xf /tmp/rust-std.tar.xz -C /tmp
        "/tmp/rust-std-${RUST_VERSION:-1.99.0}-loongarch64-unknown-linux-gnu/install.sh" --prefix="$tools" --without=rust-docs
    fi
    export CC="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-gcc"
    export CXX="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-g++"
    export AR="$tools/cross-tools/bin/loongarch64-unknown-linux-gnu-ar"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC"
    export CARGO_TARGET_LOONGARCH64_UNKNOWN_LINUX_GNU_AR="$AR"
    export QEMU="$tools/bin/qemu-loongarch64"
    # The toolchain keeps glibc under target/usr/lib64 but libgcc_s.so.1
    # under <triple>/lib. Link it into the sysroot so the QEMU prefix alone
    # resolves every test binary dependency.
    export QEMU_LD_PREFIX="$tools/cross-tools/target/usr"
    if [[ -d "$tools/cross-tools/loongarch64-unknown-linux-gnu/lib" && ! -e "$QEMU_LD_PREFIX/lib64/libgcc_s.so.1" ]]; then
        cp -L "$tools/cross-tools/loongarch64-unknown-linux-gnu/lib/libgcc_s.so.1" "$QEMU_LD_PREFIX/lib64/" 2>/dev/null || true
    fi
    build_args=()
    ;;
linux-mips64el)
    # mips64 targets left the official Rust distribution; build std from
    # source (same approach as EasyTier's mips releases).
    if [[ ! -d "$tools/lib/rustlib/src/rust" ]]; then
        curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-src-${RUST_VERSION:-1.99.0}.tar.xz" -o /tmp/rust-src.tar.xz
        tar -xf /tmp/rust-src.tar.xz -C /tmp
        "/tmp/rust-src-${RUST_VERSION:-1.99.0}/install.sh" --prefix="$tools"
    fi
    export CC="$tools/bin/mips64el-linux-gnuabi64-gcc"
    export CXX="$tools/bin/mips64el-linux-gnuabi64-g++"
    export AR="$tools/bin/mips64el-linux-gnuabi64-ar"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_LINKER="$CC"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_AR="$AR"
    export QEMU="$tools/bin/qemu-mips64el"
    export QEMU_LD_PREFIX="${YOODESK_QEMU_ROOT:-$tools/mips-root/usr/mips64el-linux-gnuabi64}"
    export RUSTC_BOOTSTRAP=1
    build_args=(-Z build-std=std,panic_abort)
    ;;
esac

rustc --version
"$QEMU" --version | head -n1

# Unit tests run under the user-mode emulator.
runner_var="CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_RUNNER"
export "$runner_var=$QEMU"

cargo build --locked --release "${build_args[@]}" --lib
cargo test --locked "${build_args[@]}" --lib

if [[ "$kind" == webservice ]]; then
    cargo build --locked --release "${build_args[@]}" --features desk-engine --bin web-service
    cargo test --locked "${build_args[@]}" --test dynamic_plugins
fi

if [[ -n "$kind" && "$kind" != webservice ]]; then
    python3.13 ci/package.py "$kind" --platform "$platform"
else
    python3.13 ci/package.py --platform "$platform"
fi

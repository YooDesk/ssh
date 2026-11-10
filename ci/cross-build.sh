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
# archive.ubuntu.com is unreliable from CI runners (connection refused or
# timeout). Canonical's Azure mirror serves the same bionic archive.
if [[ -f /etc/apt/sources.list ]]; then
    sed -i 's|http://archive.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|g; s|http://security.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|g' /etc/apt/sources.list
fi
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
# Prebuilt CPython (python-build-standalone) instead of a 13-minute source
# build: the build runs in a fresh container on every job, and the standalone
# build targets glibc 2.17+, so it runs on this 18.04 image unchanged.
if [[ ! -x "$tools/bin/python3.13" ]]; then
    python_tarball=/tmp/python-standalone.tar.gz
    python_url='https://github.com/YooDesk/desk/releases/download/sysroot-mips64el-loongnix-d10/python-3.13.18-standalone.tar.gz'
    python_sha='126343c54daff4068efc1ac269518d0cc202d57857794948103dd1454e8881be'
    curl -fL --retry 3 "$python_url" -o "$python_tarball"
    echo "$python_sha  $python_tarball" | sha256sum -c -
    mkdir -p "$tools"
    tar -xzf "$python_tarball" -C "$tools" --strip-components=1
    rm -f "$python_tarball"
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
    # under <triple>/lib. Copy it in and also expose the directory to the
    # test process, since the loader may resolve DT_NEEDED before consulting
    # the QEMU prefix.
    export QEMU_LD_PREFIX="$tools/cross-tools/target/usr"
    gcc_lib="$tools/cross-tools/loongarch64-unknown-linux-gnu/lib"
    if [[ -d "$gcc_lib" ]]; then
        mkdir -p "$QEMU_LD_PREFIX/lib64"
        cp -L "$gcc_lib/libgcc_s.so.1" "$QEMU_LD_PREFIX/lib64/" 2>/dev/null || true
    fi
    # The qemu runner needs -L explicitly: the loader resolves DT_NEEDED
    # entries (libgcc_s.so.1 among them) relative to the given root, and
    # QEMU_LD_PREFIX alone is not consulted by every loader path.
    export QEMU_RUNNER_ARGS="-L $QEMU_LD_PREFIX"
    export LD_LIBRARY_PATH="$gcc_lib:$QEMU_LD_PREFIX/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
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
    # The cross gcc's default search path omits the multiarch libdir where
    # libgcc_s.so.1 and the C library live; the final link needs it.
    # libc.so is a linker script with absolute paths (/usr/mips64el-...) and
    # libc_nonshared.a/ld.so.1 must resolve inside the extracted root, so the
    # driver needs --sysroot pointing at mips-root.
    mips_root="$tools/mips-root"
    mips_libdir="$mips_root/usr/mips64el-linux-gnuabi64/lib"
    export CARGO_TARGET_MIPS64EL_UNKNOWN_LINUX_GNUABI64_RUSTFLAGS="-C link-arg=--sysroot=$mips_root -C link-arg=-L$mips_libdir -C link-arg=-Wl,-rpath-link,$mips_libdir"
    export QEMU="$tools/bin/qemu-mips64el"
    # Debian's cross binutils keeps libopcodes/libbfd for the host in the
    # multiarch directory; `as` and `ld` fail to start without it.
    export LD_LIBRARY_PATH="$tools/mips-root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export QEMU_RUNNER_ARGS="-L ${YOODESK_QEMU_ROOT:-$tools/mips-root/usr/mips64el-linux-gnuabi64}"
    export QEMU_LD_PREFIX="${YOODESK_QEMU_ROOT:-$tools/mips-root/usr/mips64el-linux-gnuabi64}"
    export RUSTC_BOOTSTRAP=1
    build_args=(-Z build-std=std,panic_abort)
    ;;
esac

rustc --version
"$QEMU" --version | head -n1

# Unit tests run under the user-mode emulator.
runner_var="CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_RUNNER"
export "$runner_var=$QEMU $QEMU_RUNNER_ARGS"

cargo build --locked --release "${build_args[@]}" --lib
# QEMU user mode does not implement every socket option for the emulated
# kernels; on mips64el two tests that bind a real TCP listener fail with
# ENOPROTOOPT ("Protocol not available") before exercising product code.
# They pass on the native platforms and on loong64, so only skip them here.
test_args=()
if [[ "$platform" == linux-mips64el ]]; then
    # QEMU user mode rejects some socket options (ENOPROTOOPT), so tests
    # that bind a real TCP listener fail before reaching product code.
    test_args+=(--skip expired_wrong_name_client_only_certificate_still_connects_in_default_mode)
    test_args+=(--skip websocket_write_deadline_releases_stalled_connection)
    test_args+=(--skip nested_sftp_routes_reach_authentication_with_extra_capability_parameter)
    test_args+=(--skip duplex_forwarding_reads_while_target_write_is_backpressured)
fi
cargo test --locked "${build_args[@]}" --lib -- "${test_args[@]}"

if [[ "$kind" == webservice ]]; then
    cargo build --locked --release "${build_args[@]}" --features desk-engine --bin web-service
    cargo test --locked "${build_args[@]}" --test dynamic_plugins
fi

if [[ -n "$kind" && "$kind" != webservice ]]; then
    python3.13 ci/package.py "$kind" --platform "$platform"
else
    python3.13 ci/package.py --platform "$platform"
fi

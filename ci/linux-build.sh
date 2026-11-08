#!/usr/bin/env bash
set -euo pipefail
kind="$1"
platform="$2"
target="$3"
triplet="$4"
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential gcc-8 g++-8 ca-certificates curl git unzip zip xz-utils pkg-config libssl-dev zlib1g-dev libffi-dev libbz2-dev liblzma-dev libreadline-dev libsqlite3-dev autoconf automake libtool ninja-build nasm
export CC=gcc-8 CXX=g++-8
export VCPKG_MAX_CONCURRENCY=3 CARGO_BUILD_JOBS=3
mkdir -p /opt/yoodesk-tools
if [[ ! -x /opt/yoodesk-tools/bin/python3.13 ]]; then
    curl -fL --retry 3 https://www.python.org/ftp/python/3.13.5/Python-3.13.5.tgz -o /tmp/python.tgz
    tar -xf /tmp/python.tgz -C /tmp
    pushd /tmp/Python-3.13.5
    ./configure --prefix=/opt/yoodesk-tools --with-ensurepip=install
    make -j3
    make install
    popd
fi
export PATH="/opt/yoodesk-tools/bin:$PATH"
if [[ ! -x /opt/yoodesk-tools/bin/cargo ]]; then
    curl -fL --retry 3 "https://static.rust-lang.org/dist/rust-1.99.0-${target}.tar.xz" -o /tmp/rust.tar.xz
    tar -xf /tmp/rust.tar.xz -C /tmp
    "/tmp/rust-1.99.0-${target}/install.sh" --prefix=/opt/yoodesk-tools --without=rust-docs
fi
cd /workspace
git config --global --add safe.directory /workspace
export CARGO_BUILD_TARGET="$target"
if [[ "$kind" == desk ]]; then
    # System dependency list and LLVM/vcpkg pins come from RustDesk's flutter-build.yml.
    apt-get install -y --no-install-recommends clang-10 libclang-10-dev llvm-10-dev libasound2-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgtk-3-dev libpulse-dev libva-dev libxcb-randr0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxcb-shm0-dev libxdo-dev libxfixes-dev libxrandr-dev libxtst-dev libxkbcommon-dev libudev-dev libdbus-1-dev libegl1-mesa-dev libgles2-mesa-dev libgbm-dev libdrm-dev
    export LIBCLANG_PATH=/usr/lib/llvm-10/lib
    python3.13 -m pip install 'meson==1.9.0'
    if [[ ! -x /opt/yoodesk-tools/bin/cmake ]]; then
        curl -fL --retry 3 https://github.com/Kitware/CMake/releases/download/v4.3.0/cmake-4.3.0.tar.gz -o /tmp/cmake.tar.gz
        tar -xf /tmp/cmake.tar.gz -C /tmp
        pushd /tmp/cmake-4.3.0
        ./bootstrap --prefix=/opt/yoodesk-tools --parallel=3 -- -DCMAKE_USE_OPENSSL=ON
        make -j3
        make install
        popd
    fi
    python3.13 scripts/prepare_sources.py
    python3.13 scripts/prepare_linux_native.py
    export VCPKG_ROOT=/workspace/artifacts/vcpkg
    export VCPKG_INSTALLED_ROOT="$VCPKG_ROOT/installed"
    export VCPKG_DEFAULT_TRIPLET="$triplet" VCPKG_DEFAULT_HOST_TRIPLET="$triplet"
    export VCPKG_BINARY_SOURCES="clear;files,/workspace/.ci-cache/vcpkg,readwrite"
    if [[ ! -d "$VCPKG_ROOT/.git" ]]; then
        git init "$VCPKG_ROOT"
        git -C "$VCPKG_ROOT" remote add origin https://github.com/microsoft/vcpkg.git
        git -C "$VCPKG_ROOT" fetch --depth 1 origin 9e593bb18ea69cc5095e012465dcd675a822ed0d
        git -C "$VCPKG_ROOT" checkout --detach FETCH_HEAD
    fi
    # The x64 musl tool runs on glibc 2.27. On arm64 this builds the C++17 tool from source.
    # Both target dependency builds still use the Ubuntu 18.04 glibc compiler/sysroot.
    "$VCPKG_ROOT/bootstrap-vcpkg.sh" -musl -disableMetrics
    pushd artifacts/rustdesk-engine-src
    "$VCPKG_ROOT/vcpkg" install --triplet "$triplet" --host-triplet "$triplet" --x-install-root="$VCPKG_INSTALLED_ROOT"
    popd
    export PKG_CONFIG_PATH="$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig:$VCPKG_INSTALLED_ROOT/$triplet/share/pkgconfig"
    export SODIUM_LIB_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib" SODIUM_STATIC=1
    export YOODESK_STATIC_CODEC_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib"
    export YOODESK_STATIC_FFMPEG_DIR="$VCPKG_INSTALLED_ROOT/$triplet/lib"
    # libyuv's vcpkg port does not ship a pkg-config descriptor.
    mkdir -p "$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig"
    cat > "$VCPKG_INSTALLED_ROOT/$triplet/lib/pkgconfig/libyuv.pc" <<EOF
prefix=$VCPKG_INSTALLED_ROOT/$triplet
libdir=\${prefix}/lib
includedir=\${prefix}/include
Name: libyuv
Description: libyuv from the pinned RustDesk vcpkg manifest
Version: 1.0
Libs: -L\${libdir} -lyuv
Cflags: -I\${includedir}
EOF
    python3.13 scripts/build_engine.py
    export YOODESK_DESK_ENGINE_LIB_DIR="/workspace/desk-engine/target/$target/release"
fi
if [[ "$kind" == webservice ]]; then
    cargo build --locked --release --features desk-engine --bin web-service
    cargo test --locked --lib
    # The independently released VNC DLL is loaded by the real host in the regression test.
    python3.13 ci/unpack_test_plugin.py "$platform"
    export YOODESK_TEST_VNC_LIBRARY="/workspace/artifacts/test-vnc/libyoodesk_vnc.so"
    cargo test --locked --test dynamic_plugins
else
    cargo build --locked --release --lib
    cargo test --locked --lib
fi
python3.13 ci/package.py "$kind" --platform "$platform"

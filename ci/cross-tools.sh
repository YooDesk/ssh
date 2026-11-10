#!/usr/bin/env bash
# Install pinned cross toolchains and user-mode emulators for the YooDesk
# Linux ports that GitHub Actions cannot build natively.
#
# Usage: ci/cross-tools.sh <loongarch64|mips64el> <install-dir>
#
# Everything is verified with SHA256 before use. The scripts are idempotent
# so CI caches can skip downloads entirely.
set -euo pipefail
kind="$1"
prefix="$2"
mkdir -p "$prefix"

fetch() { # fetch <url> <sha256> <output>
    local url="$1" want="$2" out="$3"
    if [[ -s "$out" && "$(sha256sum "$out" | cut -d' ' -f1)" == "$want" ]]; then
        return 0
    fi
    curl -fL --retry 3 "$url" -o "$out"
    local got
    got="$(sha256sum "$out" | cut -d' ' -f1)"
    [[ "$got" == "$want" ]] || { echo "Checksum mismatch for $url: $got" >&2; exit 1; }
}

case "$kind" in
loongarch64)
    # Official Loongson x86_64 -> loongarch64 cross-tools (binutils 2.44,
    # GCC 14.2.0, glibc 2.41 sysroot). Host binary needs only glibc 2.27+.
    if [[ ! -x "$prefix/cross-tools/bin/loongarch64-unknown-linux-gnu-gcc" ]]; then
        fetch 'https://github.com/loongson/build-tools/releases/download/2025.02.21/x86_64-cross-tools-loongarch64-binutils_2.44-gcc_14.2.0-glibc_2.41.tar.xz' \
              'b6b058ab77cf21c806db409d1fb2ad4b43fa23832616216099cf8168274f16b6' \
              "$prefix/loongson-cross-tools.tar.xz"
        tar -xJf "$prefix/loongson-cross-tools.tar.xz" -C "$prefix"
        rm -f "$prefix/loongson-cross-tools.tar.xz"
    fi
    # Static user-mode emulator for tests and smoke checks (no binfmt needed).
    if [[ ! -x "$prefix/bin/qemu-loongarch64" ]]; then
        mkdir -p "$prefix/bin"
        fetch 'https://github.com/loongson/build-tools/releases/download/2025.02.21/qemu-loongarch64' \
              '794d606a2af4f6277518260eb803f8021e1a5891609fe0c52ef66f18e3e4e265' \
              "$prefix/bin/qemu-loongarch64"
        chmod +x "$prefix/bin/qemu-loongarch64"
    fi
    ;;
mips64el)
    # Debian buster cross GCC 8.3.0 with a glibc 2.28 sysroot. The buster
    # archive is frozen, so these pins are stable forever. Package set and
    # hashes verified against the archive Packages index.
    if [[ ! -x "$prefix/bin/mips64el-linux-gnuabi64-gcc" ]]; then
        mkdir -p "$prefix/debs"
        while read -r sha name; do
            fetch "https://archive.debian.org/debian/${name}" "$sha" "$prefix/debs/$(basename "$name")"
        done <<'EOF'
522c942418fc6510d717e153539e3ba72755b228c8686e83dd28a19bca5e3078 pool/main/b/binutils/binutils-mips64el-linux-gnuabi64_2.31.1-16_amd64.deb
c2058dcc74a9cf8bba4da98809ed1725e11fd34d55ddd8191a4f71ca27bf4b45 pool/main/g/gcc-8-cross/cpp-8-mips64el-linux-gnuabi64_8.3.0-2cross1_amd64.deb
a62264aa8b32c0c996b7aaed3fa03412631f7e15b0a3488c796e1759909ea69d pool/main/g/gcc-8-cross/gcc-8-mips64el-linux-gnuabi64-base_8.3.0-2cross1_amd64.deb
3c5c67a62350b5717e30ba3ce6c77556d8ea7e941ca644437e39118f8f89a8fc pool/main/g/gcc-8-cross/gcc-8-mips64el-linux-gnuabi64_8.3.0-2cross1_amd64.deb
98af5ac95e4ddcc883b601cc98c61bd20b08675f913540a09c9e1f92c853b139 pool/main/g/gcc-8-cross/libgcc-8-dev-mips64el-cross_8.3.0-2cross1_all.deb
d60da2b8d88624fe3b4d2c5c5b28f45fcff66865c34d92204a7438be80849b12 pool/main/c/cross-toolchain-base/libc6-dev-mips64el-cross_2.28-7cross1_all.deb
baed1cb18cb435f4fbb6b2d061f487823f3bcef06cb188a62d2bd0463bdd49f0 pool/main/c/cross-toolchain-base/libc6-mips64el-cross_2.28-7cross1_all.deb
f212732859f9773f289a06496230c062cd0f922e159fd189061829a7dc1f3372 pool/main/c/cross-toolchain-base/linux-libc-dev-mips64el-cross_4.19.20-1cross1_all.deb
EOF
        for deb in "$prefix"/debs/*.deb; do
            dpkg-deb -x "$deb" "$prefix/mips-root"
        done
        rm -rf "$prefix/debs"
        mkdir -p "$prefix/bin"
        for tool in gcc g++ ar ranlib ld readelf nm strip objdump objcopy; do
            if [[ -x "$prefix/mips-root/usr/bin/mips64el-linux-gnuabi64-$tool-8" ]]; then
                ln -sf "$prefix/mips-root/usr/bin/mips64el-linux-gnuabi64-$tool-8" "$prefix/bin/mips64el-linux-gnuabi64-$tool"
            else
                ln -sf "$prefix/mips-root/usr/bin/mips64el-linux-gnuabi64-$tool" "$prefix/bin/mips64el-linux-gnuabi64-$tool"
            fi
        done
    fi
    # buster's qemu-user-static ships qemu-mips64el-static; used directly as
    # the ELF interpreter because binfmt_misc is not available in the container.
    if [[ ! -x "$prefix/bin/qemu-mips64el" ]]; then
        mkdir -p "$prefix/bin"
        fetch 'https://archive.debian.org/debian/pool/main/q/qemu/qemu-user-static_3.1+dfsg-8+deb10u8_amd64.deb' \
              '34d01a0ff83d44fe4f39e35c7f86346f5e5afa3731ce7623a12688b194ea8ca3' \
              "$prefix/qemu-user-static.deb"
        dpkg-deb -x "$prefix/qemu-user-static.deb" "$prefix/qemu-root"
        cp "$prefix/qemu-root/usr/bin/qemu-mips64el-static" "$prefix/bin/qemu-mips64el"
        rm -rf "$prefix/qemu-root" "$prefix/qemu-user-static.deb"
    fi
    ;;
*)
    echo "Unknown cross kind: $kind" >&2
    exit 1
    ;;
esac

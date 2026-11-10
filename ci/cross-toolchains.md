# YooDesk cross toolchains for the Linux ports (loong64, mips64el)

All artifacts are pinned by exact URL and SHA256 (or by immutable archive
timestamp); any mismatch fails the build. Host tools run inside the Ubuntu
18.04 container, so they only need glibc 2.27 compatibility on x86_64.

## Platform targets

- `linux-loong64` — Loongson 3A5000/3C5000+ desktop and server systems
  (Loongnix, Deepin, openEuler, Kylin, Debian). Rust target:
  `loongarch64-unknown-linux-gnu` (Tier 2, official std).
- `linux-mips64el` — Loongson 2K1000 systems running Loongnix
  "DaoXiangHu" (Debian 10 fork). Rust target:
  `mips64el-unknown-linux-gnuabi64` (Tier 3; std is built from rust-src with
  `-Z build-std=std,panic_abort`, the same approach EasyTier uses).

## Toolchains (ci/cross-tools.sh)

- loongarch64: official Loongson cross-tools 2025.02.21 (binutils 2.44,
  GCC 14.2.0, glibc 2.41 sysroot, x86_64 host).
- mips64el: Debian buster cross gcc-8 8.3.0 / binutils 2.31.1 with a
  glibc 2.28 sysroot, from the frozen archive.debian.org pool.
- User-mode QEMU for tests and smoke checks: Loongson's static
  qemu-loongarch64 and buster's qemu-mips64el-static. Invoked explicitly
  (no binfmt_misc in the container).

## Foreign sysroots (ci/cross-sysroot.sh — desk engine only)

The desk engine (RustDesk tree) links dynamically against the target
system's media stack through the `linux-pkg-config` feature chain:

- loongarch64: Debian sid loong64 packages from snapshot.debian.org at a
  pinned timestamp (reproducible). ffmpeg 9, libyuv, x264/x265, libvpx,
  opus, sodium, gtk3, gstreamer, pulse, alsa, va, vdpau, dbus, X11, drm,
  gbm — the complete set. glibc baseline 2.43.
- mips64el: Loongnix "DaoXiangHu" pool (frozen; the archive's apt indexes
  are empty, so packages are fetched by direct URL with recorded SHA256 —
  see cross-sysroot-mips64el.debs). ffmpeg 4.1.6, x264/x265, aom, opus,
  gtk3, gstreamer 1.14, pulse, alsa, dbus — the exact versions shipping on
  Loongson 2K systems. libyuv is missing from that archive and is built
  from source with the cross GCC (at the same commit the desk vcpkg
  manifest pins).

## bindgen strategy

hwcodec and scrap run bindgen 0.59 against libclang 10 (Ubuntu 18.04).
- mips64el: clang 10 knows the target; `--target=mips64el-linux-gnuabi64`.
- loongarch64: clang 10 predates LoongArch. Every bindgen'd header set in
  the tree (vpx, aom, yuv, ffmpeg_ffi, mux_ffi) is plain C with no
  arch-conditional layout, and loongarch64 is LP64 little-endian exactly
  like x86_64, so headers are parsed with `--target=x86_64` — the
  generated layouts match.

## glibc baselines per platform

- linux-x64 / linux-arm64: 2.27 (Ubuntu 18.04, unchanged)
- linux-loong64: 2.43 (Debian sid sysroot; every LoongArch distro ≥ 2.36)
- linux-mips64el: 2.28 (Debian buster / Loongnix DaoXiangHu)

## Verification performed before pinning (2026-11-10)

- ring 0.17.14 (TLS for every Rust component) falls back to its portable
  C implementation on unknown architectures (`asm_target = None` outside
  x86/x86_64/arm/aarch64; `sources_for_arch` then takes only the
  arch-independent C sources). No loongarch/mips assembly is referenced.
- openssl-src 300.5.3 (vendored openssl 3.5.4, used by hbb_common via
  reqwest native-tls) has explicit targets for both
  `loongarch64-unknown-linux-gnu` (linux-generic64) and
  `mips64el-unknown-linux-gnuabi64` (linux64-mips64).
- libsodium-sys 0.2.7 cross-builds its bundled source with CC/--host and
  skips `make check` when cross-compiling.
- libvpx 1.8.2 `configure` auto-detects `mips64el-linux-gnuabi64` from CC
  and endianness from a compiled probe; no explicit target string needed.
- The Loongnix mips64el pool really ships the pinned versions (checked by
  direct download + sha256); its apt indexes are empty, hence URL pinning.
- snapshot.debian.org serves the full sid loong64 archive at
  20261108T000000Z (both the Packages index and every resolved deb).

## CI environment requirements for the cross builds

Python 3.13.5 is built from source inside the Ubuntu 18.04 container. It
needs the same development headers the native flow installs, or its
stdlib modules silently drop out and `ensurepip` fails:

    zlib1g-dev libffi-dev libssl-dev libbz2-dev liblzma-dev     libreadline-dev libsqlite3-dev

Symptom when missing: `zipimport.ZipImportError: can't decompress data;
zlib not available` and `SSL module is not available` from pip. The
container's `ubuntu:18.04` image has none of these.

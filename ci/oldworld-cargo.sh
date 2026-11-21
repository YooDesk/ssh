#!/usr/bin/env bash
# Keep upstream locked crates: the vendor Cargo hardcodes a registry rewrite.
set -euo pipefail
tools="${1:?tool directory required}"
archive="${2:-$tools/upstream-cargo.tar.xz}"
sha=d7674918d28093097614cd9728b6ca60db9ea3038f640f0bd1e9a4188c7568ce
if [[ ! -f "$archive" ]] || ! echo "$sha  $archive" | sha256sum -c - >/dev/null 2>&1; then
    curl -fL --retry 3 --connect-timeout 30 \
        'https://static.rust-lang.org/dist/cargo-1.99.0-x86_64-unknown-linux-gnu.tar.xz' -o "$archive"
    echo "$sha  $archive" | sha256sum -c -
fi
if [[ ! -f "$tools/upstream-cargo/.source-$sha" ]]; then
    tar -xJf "$archive" -C "$tools"
    bash "$tools/cargo-1.99.0-x86_64-unknown-linux-gnu/install.sh" \
        --prefix="$tools/upstream-cargo" --disable-ldconfig
    touch "$tools/upstream-cargo/.source-$sha"
fi
"$tools/upstream-cargo/bin/cargo" --version

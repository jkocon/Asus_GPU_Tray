#!/usr/bin/env bash
# Builds the binary as a regular user (never cargo as root) and prints its path. Called by
# install.sh: BIN=$("$SRC/build.sh"). As root it builds as $SUDO_USER, or as the user named in
# ASUS_GPU_TRAY_BUILD_USER (cachyos-sync on machines updated by cachyos_sync).
set -euo pipefail
crate=$(cd "$(dirname "$0")" && pwd)
command -v cargo >/dev/null || { echo "build.sh: cargo is missing (Arch: pacman -S rust)" >&2; exit 1; }
cargo=(cargo build --release --locked --quiet --manifest-path "$crate/Cargo.toml")
if [[ $EUID -eq 0 ]]; then
    user=${ASUS_GPU_TRAY_BUILD_USER:-${SUDO_USER:-}}
    [[ -n $user && $user != root ]] || { echo "build.sh: run through sudo, or set ASUS_GPU_TRAY_BUILD_USER" >&2; exit 1; }
    home=$(getent passwd "$user" | cut -d: -f6)
    [[ -n $home ]] || { echo "build.sh: no user $user" >&2; exit 1; }
    target=$home/.cache/asus-gpu-tray-build
    runuser -u "$user" -- env HOME="$home" CARGO_TARGET_DIR="$target" "${cargo[@]}" >&2
else
    target=${CARGO_TARGET_DIR:-$crate/target}
    "${cargo[@]}" >&2
fi
echo "$target/release/asus-gpu-tray"

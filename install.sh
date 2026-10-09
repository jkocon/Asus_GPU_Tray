#!/usr/bin/env bash
# Installs or updates Asus GPU Tray. Run as root: sudo ./install.sh
# The tray itself is installed everywhere. The hardware switch backend (XG Mobile / MUX:
# scripts, systemd units, polkit rule) is installed only on ASUS laptops with the
# asus-armoury driver. GPU access modes come from cardwire when it is installed.
#
# The binary: ./asus-gpu-tray when it is there (the release archive), else built from this
# checkout with build.sh (needs cargo; the build runs as the user behind sudo, never as root).
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
LIB=/usr/local/lib/asus-gpu-tray
[[ $EUID -eq 0 ]] || { echo "Run as root (sudo ./install.sh)"; exit 1; }

warn() { echo "warning: $*" >&2; }
[[ -r /usr/share/hwdata/pci.ids ]] || warn "/usr/share/hwdata/pci.ids is missing (package hwdata) - GPU names fall back to PCI IDs"
command -v cardwire >/dev/null || warn "cardwire is not installed - the Integrated/Hybrid/Smart modes will not be offered"

if [[ -x $SRC/asus-gpu-tray ]]; then
    BIN=$SRC/asus-gpu-tray
else
    BIN=$("$SRC/build.sh")
fi
"$BIN" --version >/dev/null  # runs on this system (glibc, libraries)

# Version 1.x: the Python tray and the shell helpers; phase 3 of the rewrite: wrappers in place of
# the helpers, the originals in legacy/, the binary in rust/.
rm -rf "$LIB/asus_gpu_tray.py" "$LIB/__pycache__" "$LIB/legacy" "$LIB/rust" "$LIB/nvidia.svg" "$LIB/asus-gpu-tray.svg"
rm -f "$LIB/asus-gpu-switch-reboot" "$LIB/asus-gpu-switch-apply" "$LIB/asus-gpu-switch-live" "$LIB/asus-gpu-egpu-power"
# Root runs it, so it is root's and not writable by the user who built it.
install -Dm755 -o root -g root "$BIN" "$LIB/asus-gpu-tray.new"
mv -f "$LIB/asus-gpu-tray.new" "$LIB/asus-gpu-tray"  # a running tray keeps its old copy
rm -rf "$LIB/locale"
if command -v msgfmt >/dev/null; then
    for po in "$SRC"/po/*.po; do
        lang=$(basename "$po" .po)
        install -d "$LIB/locale/$lang/LC_MESSAGES"
        msgfmt -o "$LIB/locale/$lang/LC_MESSAGES/asus-gpu-tray.mo" "$po"
    done
else
    warn "msgfmt (package gettext) is missing - the tray stays in English"
fi
install -Dm644 "$SRC/desktop/asus-gpu-tray-autostart.desktop" /etc/xdg/autostart/asus-gpu-tray.desktop
install -Dm644 "$SRC/desktop/asus-gpu-tray.desktop" /usr/local/share/applications/asus-gpu-tray.desktop
install -Dm644 "$SRC/icons/asus-gpu-tray.svg" /usr/local/share/icons/hicolor/scalable/apps/asus-gpu-tray.svg
gtk-update-icon-cache -qtf /usr/local/share/icons/hicolor 2>/dev/null || true

if [[ -e /sys/class/firmware-attributes/asus-armoury/attributes/egpu_enable ]]; then
    command -v setpci >/dev/null || warn "setpci (package pciutils) is missing - switching without a reboot will refuse to run"
    install -Dm644 -t /etc/systemd/system "$SRC/systemd/asus-gpu-switch@.service" "$SRC/systemd/asus-gpu-switch-apply.service" "$SRC/systemd/asus-gpu-live@.service"
    install -Dm644 -t /etc/polkit-1/rules.d "$SRC/polkit/50-asus-gpu-tray.rules"
    install -Dm644 -t /etc/udev/rules.d "$SRC/udev/72-asus-gpu-tray-egpu.rules"
    udevadm control --reload
    "$LIB/asus-gpu-tray" egpu-power  # an XG Mobile that is already active
    systemctl daemon-reload
    systemctl enable asus-gpu-switch-apply.service
    if [[ -e /usr/lib/systemd/user/plasma-login-kwin_wayland.service ]]; then
        # Keeps the login screen on the iGPU once the KDE setup's iGPU link exists (see README).
        install -Dm755 "$SRC/kde/60-asus-gpu-tray-greeter" /etc/systemd/user-environment-generators/60-asus-gpu-tray-greeter
    fi
    echo "Hardware switch backend installed (ASUS asus-armoury detected)"
    echo "Switching the XG Mobile without a reboot also needs the one-time KDE setup, see README."
else
    echo "No asus-armoury egpu_enable attribute - installed the tray only"
fi

echo "Asus GPU Tray $("$LIB/asus-gpu-tray" --version | cut -d' ' -f2) installed. It starts on login; a running tray"
echo "is replaced at the next login (or now: pkill -x asus-gpu-tray; pkill -f asus_gpu_tray.py; $LIB/asus-gpu-tray &)."

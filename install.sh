#!/usr/bin/env bash
# Installs or updates Asus GPU Tray. Run as root: sudo ./install.sh
# The tray itself is installed everywhere. The hardware switch backend (XG Mobile / MUX:
# scripts, systemd units, polkit rule) is installed only on ASUS laptops with the
# asus-armoury driver. GPU access modes come from cardwire when it is installed.
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
LIB=/usr/local/lib/asus-gpu-tray
[[ $EUID -eq 0 ]] || { echo "Run as root (sudo ./install.sh)"; exit 1; }

warn() { echo "warning: $*" >&2; }
python3 -c 'import PyQt6.QtWidgets' 2>/dev/null || warn "PyQt6 is missing (Arch: python-pyqt6, Debian/Ubuntu: python3-pyqt6)"
[[ -r /usr/share/hwdata/pci.ids ]] || warn "/usr/share/hwdata/pci.ids is missing (package hwdata) - GPU names fall back to PCI IDs"
command -v cardwire >/dev/null || warn "cardwire is not installed - the Integrated/Hybrid/Smart modes will not be offered"

install -Dm755 "$SRC/asus_gpu_tray.py" "$LIB/asus_gpu_tray.py"
install -Dm644 -t "$LIB" "$SRC/icons/nvidia.svg" "$SRC/icons/asus-gpu-tray.svg"
install -Dm644 "$SRC/desktop/asus-gpu-tray-autostart.desktop" /etc/xdg/autostart/asus-gpu-tray.desktop
install -Dm644 "$SRC/desktop/asus-gpu-tray.desktop" /usr/local/share/applications/asus-gpu-tray.desktop
install -Dm644 "$SRC/icons/asus-gpu-tray.svg" /usr/local/share/icons/hicolor/scalable/apps/asus-gpu-tray.svg
gtk-update-icon-cache -qtf /usr/local/share/icons/hicolor 2>/dev/null || true

if [[ -e /sys/class/firmware-attributes/asus-armoury/attributes/egpu_enable ]]; then
    command -v setpci >/dev/null || warn "setpci (package pciutils) is missing - switching without a reboot will refuse to run"
    install -Dm755 -t "$LIB" "$SRC/scripts/asus-gpu-switch-reboot" "$SRC/scripts/asus-gpu-switch-apply" "$SRC/scripts/asus-gpu-switch-live"
    install -Dm644 -t /etc/systemd/system "$SRC/systemd/asus-gpu-switch@.service" "$SRC/systemd/asus-gpu-switch-apply.service" "$SRC/systemd/asus-gpu-live@.service"
    install -Dm644 -t /etc/polkit-1/rules.d "$SRC/polkit/50-asus-gpu-tray.rules"
    systemctl daemon-reload
    systemctl enable asus-gpu-switch-apply.service
    echo "Hardware switch backend installed (ASUS asus-armoury detected)"
    echo "Switching the XG Mobile without a reboot also needs the one-time KDE setup, see README."
else
    echo "No asus-armoury egpu_enable attribute - installed the tray only"
fi

echo "Asus GPU Tray installed. It starts on login; to start it now: python3 $LIB/asus_gpu_tray.py &"

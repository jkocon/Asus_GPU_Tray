#!/usr/bin/env bash
# Installs or updates Asus GPU Tray. Run as root: sudo ./install.sh
# The tray itself is installed everywhere. The reboot-based hardware switch backend
# (XG Mobile / MUX: systemd units + polkit rule) is installed only on ASUS laptops
# with the asus-armoury driver. Live GPU modes come from cardwire when it is installed.
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
LIB=/usr/local/lib/asus-gpu-tray
[[ $EUID -eq 0 ]] || { echo "Run as root (sudo ./install.sh)"; exit 1; }

install -Dm755 "$SRC/asus_gpu_tray.py" "$LIB/asus_gpu_tray.py"
install -Dm644 -t "$LIB" "$SRC/icons/nvidia.svg" "$SRC/icons/asus-gpu-tray.svg"
install -Dm644 "$SRC/desktop/asus-gpu-tray-autostart.desktop" /etc/xdg/autostart/asus-gpu-tray.desktop
install -Dm644 "$SRC/desktop/asus-gpu-tray.desktop" /usr/local/share/applications/asus-gpu-tray.desktop
install -Dm644 "$SRC/icons/asus-gpu-tray.svg" /usr/local/share/icons/hicolor/scalable/apps/asus-gpu-tray.svg
gtk-update-icon-cache -qtf /usr/local/share/icons/hicolor 2>/dev/null || true

if [[ -e /sys/class/firmware-attributes/asus-armoury/attributes/egpu_enable ]]; then
    install -Dm755 -t "$LIB" "$SRC/scripts/asus-gpu-switch-reboot" "$SRC/scripts/asus-gpu-switch-apply" "$SRC/scripts/asus-gpu-switch-live"
    install -Dm644 -t /etc/systemd/system "$SRC/systemd/asus-gpu-switch@.service" "$SRC/systemd/asus-gpu-switch-apply.service" "$SRC/systemd/asus-gpu-live@.service"
    install -Dm644 -t /etc/polkit-1/rules.d "$SRC/polkit/50-asus-gpu-tray.rules"
    systemctl daemon-reload
    systemctl enable asus-gpu-switch-apply.service
    echo "Reboot-based switch backend installed (ASUS asus-armoury detected)"
else
    echo "No asus-armoury/egpu_enable - tray only (live switching via cardwire, or supergfxctl -m if available)"
fi

echo "Asus GPU Tray installed. It starts on login; to start it now: python3 $LIB/asus_gpu_tray.py &"

#!/usr/bin/env bash
# Removes Asus GPU Tray. Run as root: sudo ./uninstall.sh
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "Run as root (sudo ./uninstall.sh)"; exit 1; }

systemctl disable asus-gpu-switch-apply.service 2>/dev/null || true
rm -f /etc/systemd/system/asus-gpu-switch@.service \
      /etc/systemd/system/asus-gpu-switch-apply.service \
      /etc/systemd/system/asus-gpu-live@.service \
      /etc/polkit-1/rules.d/50-asus-gpu-tray.rules \
      /etc/xdg/autostart/asus-gpu-tray.desktop \
      /usr/local/share/applications/asus-gpu-tray.desktop \
      /usr/local/share/icons/hicolor/scalable/apps/asus-gpu-tray.svg \
      /etc/modprobe.d/zz-asus-gpu-tray-switch.conf
rm -rf /usr/local/lib/asus-gpu-tray /var/lib/asus-gpu-tray
systemctl daemon-reload
gtk-update-icon-cache -qtf /usr/local/share/icons/hicolor 2>/dev/null || true
echo "Asus GPU Tray removed"

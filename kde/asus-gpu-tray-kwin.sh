# Asus GPU Tray (live XG Mobile switch): KWin uses only the iGPU and Mesa's EGL, so it never holds
# the NVIDIA card open and the card can be unplugged in software. Copy to
# ~/.config/plasma-workspace/env/ (with asus-gpu-tray.conf as the KWin drop-in); remove both and
# log in again to undo.
# /dev/dri/asus-igpu-card comes from /etc/udev/rules.d/70-asus-igpu-card.rules. Not in MUX mode:
# the panel is wired to the dGPU then, and KWin on the iGPU would find no outputs (black screen).
env_file="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/asus-gpu-tray-kwin.env"
rm -f "$env_file"
if [ -e /dev/dri/asus-igpu-card ] &&
    [ "$(cat /sys/class/firmware-attributes/asus-armoury/attributes/gpu_mux_mode/current_value 2>/dev/null)" != 0 ]; then
    export KWIN_DRM_DEVICES=/dev/dri/asus-igpu-card
    # Read by the KWin service only (asus-gpu-tray.conf), so apps still get NVIDIA's EGL.
    echo __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json > "$env_file"
fi

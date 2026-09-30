# Asus GPU Tray

A system tray icon for Linux that shows which GPU is currently rendering and switches
[supergfxd](https://gitlab.com/asus-linux/supergfxctl) graphics modes, including the ASUS
**XG Mobile** external GPU dock. It detects the installed graphics cards by itself and only offers
the modes your hardware supports.

ROG Control Center (asusctl 6.5) has no eGPU switch, which is why this tool exists.

Developed and tested on an ASUS ROG Flow X16 GV601RE (Radeon 680M + RTX 3050 Ti, XG Mobile with
RTX 3070) running CachyOS with KDE Plasma.

## Features

- Lists every graphics card found in sysfs and classifies it as **iGPU**, **dGPU** or **eGPU**.
- Shows the card's name (from `hwdata`'s `pci.ids`), bound driver and runtime power state.
- Reads the available modes from `supergfxctl -s` (`Integrated`, `Hybrid`, `AsusEgpu`,
  `AsusMuxDgpu`, `Vfio`, `NvidiaNoModeset`) and marks the current one.
- Shows XG Mobile status and options only on ASUS laptops that expose `asus-armoury` firmware attributes.
- Safe, reboot-based mode switching on ASUS laptops (see below).
- Without supergfxd it still works as a read-only GPU viewer.
- Never wakes a runtime-suspended dGPU: it reads only kernel-cached sysfs files, never `lspci`,
  `nvidia-smi` or `/dev/nvidia*`. (Waking the dGPU makes ROG Control Center spam
  "dGPU status changed" notifications.)

## Icon

| Icon | Meaning |
|---|---|
| NVIDIA logo | an NVIDIA card is rendering |
| red **AMD** | an AMD card is rendering |
| blue **Intel** | an Intel card is rendering |
| grey **GPU** / **?** | unknown vendor / no GPU detected |
| purple dot | the rendering card is external (eGPU / XG Mobile) |

Hover to see the mode and XG Mobile status, left-click for a notification with the active GPU,
right-click for the menu. Every mode change asks for confirmation first.

## How GPUs are classified

| Kind | Rule |
|---|---|
| iGPU | Intel card on PCI bus 0, or an AMD card whose PCI bus also contains USB controllers (an APU) |
| eGPU | an ancestor PCIe port has `external_facing = 1` or the device is `removable` (Thunderbolt / USB4), or it is an NVIDIA card while ASUS `egpu_enable = 1` (XG Mobile) |
| dGPU | anything else |

## Switching modes

supergfxd's own switching (`supergfxctl -m`) works live: on logout it unloads the NVIDIA driver,
re-routes the GPU and loads the driver again. With nvidia-open 615 this **froze the system** on
`rmmod nvidia` (last log line: `nvidia-modeset: Unloading`), in every direction.

So on ASUS laptops with `asus-armoury`, `install.sh` adds a reboot-based backend that applies the
change during boot, before the NVIDIA driver is loaded:

```
menu click
  └─ systemctl start asus-gpu-switch@<Mode>.service   (polkit: no password, only these 4 modes)
       └─ asus-gpu-switch-reboot <Mode>
            ├─ writes /var/lib/asus-gpu-tray/pending
            ├─ creates /etc/modprobe.d/zz-asus-gpu-tray-switch.conf (blacklists nvidia for one boot)
            ├─ for MUX: writes gpu_mux_mode (firmware applies it after reboot)
            └─ systemctl reboot
boot
  └─ asus-gpu-switch-apply.service   (before supergfxd and the display manager)
       ├─ removes the NVIDIA devices from PCI (safe: the driver is not loaded)
       ├─ writes egpu_enable (1 = XG Mobile, 0 = built-in dGPU) + dgpu_disable=0
       ├─ rescans PCI so the right card appears
       ├─ sets "mode" in /etc/supergfxd.conf
       └─ always removes the blacklist and the pending file
  └─ supergfxd starts in the new mode and loads the NVIDIA driver
```

`egpu_enable` persists in firmware across reboots. At boot supergfxd itself detects
`egpu_enable = 1` and switches to `AsusEgpu`.

On other machines, or when the backend is not installed, the tray calls `supergfxctl -m <mode>`
and shows the action supergfxd asks for (log out or reboot). On ASUS laptops it warns about the
freeze first.

### Safeguards

- `AsusEgpu` cannot be selected unless the XG Mobile is connected and locked (`egpu_connected = 1`).
  Both the menu and the scripts check this.
- If the XG Mobile is disconnected at boot, `asus-gpu-switch-apply` sets `Hybrid` instead.
- If the `nvidia` driver is somehow already loaded, `asus-gpu-switch-apply` leaves the hardware alone
  and the system boots in the old mode. The blacklist is removed either way.
- Switch back to `Hybrid` **before** undocking the XG Mobile.

## Requirements

- Python 3.10+ with PyQt6 (`python-pyqt6` on Arch)
- `hwdata` (for `/usr/share/hwdata/pci.ids`)
- `supergfxctl` / supergfxd for mode switching (optional)
- A kernel with the `asus-armoury` driver for XG Mobile, MUX and the reboot backend (optional)
- A desktop with a system tray (tested on KDE Plasma)

## Installation

```bash
git clone https://github.com/jkocon/Asus_GPU_Tray.git
cd Asus_GPU_Tray
sudo ./install.sh
```

The tray starts on the next login. To start it now: `python3 /usr/local/lib/asus-gpu-tray/asus_gpu_tray.py &`.
After closing it, start it again from the application menu (**Asus GPU Tray**, System category).
Only one instance runs per session.

To try it without installing: `python3 asus_gpu_tray.py`. Use `python3 asus_gpu_tray.py --dump` to
print what was detected without the GUI.

Installed files:

| File | Purpose |
|---|---|
| `/usr/local/lib/asus-gpu-tray/asus_gpu_tray.py` | tray application |
| `/usr/local/lib/asus-gpu-tray/asus-gpu-switch-reboot` | schedules the change and reboots (ASUS only) |
| `/usr/local/lib/asus-gpu-tray/asus-gpu-switch-apply` | applies the change at boot (ASUS only) |
| `/etc/systemd/system/asus-gpu-switch@.service` | runs `asus-gpu-switch-reboot` (ASUS only) |
| `/etc/systemd/system/asus-gpu-switch-apply.service` | runs `asus-gpu-switch-apply`, enabled (ASUS only) |
| `/etc/polkit-1/rules.d/50-asus-gpu-tray.rules` | lets the `wheel` group start `asus-gpu-switch@<mode>` without a password (ASUS only) |
| `/etc/xdg/autostart/asus-gpu-tray.desktop` | autostart on login |
| `/usr/local/share/applications/asus-gpu-tray.desktop` | application menu entry |
| `/usr/local/share/icons/hicolor/scalable/apps/asus-gpu-tray.svg` | application icon |

Uninstall: `sudo ./uninstall.sh`.

## Troubleshooting

```bash
python3 asus_gpu_tray.py --dump                  # detected GPUs, modes and backend
journalctl -b -u asus-gpu-switch-apply           # what was done at the last boot
journalctl -b -1 -u 'asus-gpu-switch@*'          # scheduling of the change (previous boot)
supergfxctl -g                                   # current mode
cat /sys/class/firmware-attributes/asus-armoury/attributes/{egpu_connected,egpu_enable}/current_value
```

Cancel a scheduled change manually (before rebooting):
`sudo rm /var/lib/asus-gpu-tray/pending /etc/modprobe.d/zz-asus-gpu-tray-switch.conf`.

## Limitations

- The classification heuristics have only been tested on the ROG Flow X16 GV601RE.
  Thunderbolt eGPUs and Intel laptops are untested - reports and `--dump` output are welcome.
- If a laptop has several cards of the same kind, only the first one is used for mode labels.
- The reboot backend only handles NVIDIA dGPUs/eGPUs, which covers all XG Mobile models.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). The NVIDIA logo shape in `icons/nvidia.svg` comes from the
[char-white](https://github.com/CachyOS/char-white) icon theme (GPL).

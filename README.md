# Asus GPU Tray

A system tray icon for Linux that shows which GPU is currently rendering and switches GPU modes:

- **live** GPU access modes (Integrated / Hybrid / Smart) through [cardwire](https://github.com/OpenGamingCollective/cardwire),
  with no reboot or logout;
- **hardware** modes on ASUS laptops (built-in dGPU / **XG Mobile** eGPU dock / MUX), which cardwire
  does not handle, applied safely during the next boot.

It detects the installed graphics cards by itself and only offers what your hardware supports.

![Tray icon and menu](docs/menu.png)

ROG Control Center (asusctl 6.5) has no eGPU switch, and supergfxctl is deprecated in favour of
cardwire, which does not support eGPUs yet. That gap is why this tool exists.

Developed and tested on an ASUS ROG Flow X16 GV601RE (Radeon 680M + RTX 3050 Ti, XG Mobile with
RTX 3070) running CachyOS with KDE Plasma.

## Features

- Lists every graphics card and classifies it as **iGPU**, **dGPU** or **eGPU**, with its name
  (from `hwdata`'s `pci.ids`), driver, runtime power state and whether cardwire blocks it.
- **GPU access (live, cardwire):** Integrated blocks the dGPU/eGPU for new apps, Hybrid allows all
  GPUs, Smart allows the dGPU only for approved apps. Uses `cardwire set <mode>`, takes effect
  immediately; already running apps keep their GPU until restarted.
- **Hardware (reboot, ASUS only):** built-in dGPU, XG Mobile, dGPU-only MUX. XG Mobile is greyed out
  until the dock is connected and locked.
- Falls back to supergfxd modes when cardwire is not installed, and works as a read-only GPU viewer
  without either.
- Never wakes a runtime-suspended dGPU: it reads only kernel-cached sysfs files and cardwire's device
  list, never `lspci`, `nvidia-smi` or `/dev/nvidia*`. (Waking the dGPU makes ROG Control Center spam
  "dGPU status changed" notifications.)

## Icon

| Icon | Meaning |
|---|---|
| NVIDIA logo | an NVIDIA card is rendering |
| red **AMD** | an AMD card is rendering |
| blue **Intel** | an Intel card is rendering |
| grey **GPU** / **?** | unknown vendor / no GPU detected |
| purple dot | an external GPU is attached (XG Mobile mode / Thunderbolt eGPU) |

"Rendering" means the dGPU/eGPU is awake, has a driver and is not blocked; otherwise the iGPU is
shown (or the dGPU in MUX mode, where it drives the panel). Hover for details, left-click for a
notification with the active GPU, right-click for the menu.

## How GPUs are classified

| Kind | Rule |
|---|---|
| iGPU | Intel card on PCI bus 0, or an AMD card whose PCI bus also contains USB controllers (an APU) |
| eGPU | an ancestor PCIe port has `external_facing = 1` or the device is `removable` (Thunderbolt / USB4), or it is an NVIDIA card while ASUS `egpu_enable = 1` (XG Mobile) |
| dGPU | anything else |

cardwire hides a blocked GPU's sysfs files from every process (even root), so blocked cards are
taken from `cardwire list --json` instead.

## Hardware switching (ASUS)

supergfxd's live switching unloads the NVIDIA driver on logout. With nvidia-open 615 this
**froze the system** on `rmmod nvidia` (last log line: `nvidia-modeset: Unloading`), in every
direction. So hardware changes are applied during boot, before the NVIDIA driver is loaded:

```
menu click
  └─ systemctl start asus-gpu-switch@<Mode>.service   (polkit: no password, only these 4 modes)
       └─ asus-gpu-switch-reboot <Mode>
            ├─ writes /var/lib/asus-gpu-tray/pending
            ├─ creates /etc/modprobe.d/zz-asus-gpu-tray-switch.conf (blacklists nvidia for one boot)
            ├─ for MUX: writes gpu_mux_mode (firmware applies it after reboot)
            └─ systemctl reboot
boot
  └─ asus-gpu-switch-apply.service   (before supergfxd, cardwired and the display manager)
       ├─ removes the NVIDIA devices from PCI (safe: the driver is not loaded)
       ├─ writes egpu_enable (1 = XG Mobile, 0 = built-in dGPU) + dgpu_disable=0
       ├─ rescans PCI so the right card appears
       ├─ sets "mode" in /etc/supergfxd.conf, if supergfxd is installed
       └─ always removes the blacklist and the pending file; without supergfxd it replays
          udev events so the NVIDIA driver loads for the new card
```

It must run before cardwired: once cardwire blocks a GPU, the script could no longer see it in sysfs.
`egpu_enable` persists in firmware across reboots.

### Safeguards

- XG Mobile cannot be selected unless the dock is connected and locked (`egpu_connected = 1`).
  Both the menu and the scripts check this.
- If the XG Mobile is disconnected at boot, `asus-gpu-switch-apply` switches to the built-in dGPU instead.
- If the `nvidia` driver is somehow already loaded, `asus-gpu-switch-apply` leaves the hardware alone
  and the system boots in the old mode. The blacklist is removed either way.
- Switch back to the built-in dGPU **before** undocking the XG Mobile.

## Requirements

- Python 3.10+ with PyQt6 (`python-pyqt6` on Arch)
- `hwdata` (for `/usr/share/hwdata/pci.ids`)
- [cardwire](https://github.com/OpenGamingCollective/cardwire) for live GPU modes (recommended;
  needs a kernel with BPF LSM enabled and Wayland), or supergfxd as a fallback
- A kernel with the `asus-armoury` driver for XG Mobile, MUX and the reboot backend (optional)
- A desktop with a system tray (tested on KDE Plasma)

## Installation

```bash
git clone https://github.com/jkocon/Asus_GPU_Tray.git
cd Asus_GPU_Tray
sudo ./install.sh
```

Install cardwire separately, following its
[installation guide](https://opengamingcollective.github.io/cardwire/getting-started/installation.html).

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
python3 asus_gpu_tray.py --dump                  # detected GPUs, modes and backends
cardwire get; cardwire list                      # cardwire mode and blocked GPUs
journalctl -b -u asus-gpu-switch-apply           # what was done at the last boot
journalctl -b -1 -u 'asus-gpu-switch@*'          # scheduling of the change (previous boot)
cat /sys/class/firmware-attributes/asus-armoury/attributes/{egpu_connected,egpu_enable,gpu_mux_mode}/current_value
```

Cancel a scheduled hardware change manually (before rebooting):
`sudo rm /var/lib/asus-gpu-tray/pending /etc/modprobe.d/zz-asus-gpu-tray-switch.conf`.

## Limitations

- The classification heuristics have only been tested on the ROG Flow X16 GV601RE.
  Thunderbolt eGPUs and Intel laptops are untested - reports and `--dump` output are welcome.
- cardwire is in early development; its CLI output may change between versions.
- If a laptop has several cards of the same kind, only the first one is used for labels.
- The reboot backend only handles NVIDIA dGPUs/eGPUs, which covers all XG Mobile models.

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). The NVIDIA logo shape in `icons/nvidia.svg` comes from the
[char-white](https://github.com/CachyOS/char-white) icon theme (GPL).

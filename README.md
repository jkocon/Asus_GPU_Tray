# Asus GPU Tray

**A system tray app for ASUS ROG laptops on Linux that shows which GPU is working and switches
between the built-in dGPU and the XG Mobile eGPU dock without a reboot.**

![Tray icon and menu](docs/menu.png)

- **XG Mobile ⇄ built-in dGPU, live.** One click and about 40 seconds. No reboot, no logout.
- **GPU access modes via [cardwire](https://github.com/OpenGamingCollective/cardwire).**
  Integrated, Hybrid and Smart switch instantly.
- **MUX (dGPU-only) mode.** Applied safely during the next boot.
- **Detects your hardware.** It finds the iGPU, dGPU and eGPU and offers only the modes your
  laptop supports.
- **Never wakes a sleeping dGPU just to show its status.**

## Why this exists

The XG Mobile is not a normal eGPU. ASUS firmware moves the laptop's PCIe lanes from the built-in
GPU to the dock (`egpu_enable`). The built-in card disappears and the dock's card appears at the
same PCI address. On Linux nothing did this reliably:

| Tool | XG Mobile | What happened |
|---|---|---|
| ROG Control Center (asusctl 6.5) | – | no eGPU switch at all |
| supergfxctl (deprecated) | on logout | unloads the NVIDIA driver; with nvidia-open 615 this froze the system every time |
| cardwire | – | only decides which GPUs apps may use; it never switches hardware (eGPU is on its 1.0 roadmap) |
| writing `egpu_enable` yourself | – | the laptop resets instantly ("data fabric sync flood") |

Asus GPU Tray uses cardwire for what it does well and adds the missing hardware switch. The
[technical write-up](docs/how-it-works.md) explains how the switch is made safe and how the
problems were tracked down.

## Tested hardware

| Laptop | GPUs | What was tested | Software |
|---|---|---|---|
| ASUS ROG Flow X16 (2022) GV601RE | Radeon 680M + RTX 3050 Ti | live and reboot switch, both directions; cardwire modes | CachyOS, kernel 7.2, nvidia-open 615, KDE Plasma 6 (Wayland), cardwire 0.12 |
| ASUS ROG Flow X13 (2021) GV301QE | Radeon Vega (Ryzen 5000) + RTX 3050 Ti | live switch, both directions (~35 s) | CachyOS, kernel 7.2, nvidia-open 615, KDE Plasma 6 (Wayland), supergfxd 5.2 running, no cardwire |

Dock: XG Mobile with RTX 3070 (the same unit on both laptops). On both, the NVIDIA GPU sits behind
root port `00:01.1`.

Other ROG laptops with XG Mobile support (Flow Z13, …) should work if the kernel exposes
the `asus-armoury` firmware attributes. On any Linux laptop the tray shows your GPUs and the
cardwire modes. Please report your results. The output of `python3 asus_gpu_tray.py --dump` helps
most.

## Requirements

| Needed for | Requirement |
|---|---|
| the tray | Python ≥ 3.10, PyQt6 (`python-pyqt6` / `python3-pyqt6`), `hwdata` (GPU names), a system tray (tested on KDE Plasma) |
| GPU access modes | [cardwire](https://opengamingcollective.github.io/cardwire/getting-started/installation.html) (needs BPF LSM and Wayland) |
| hardware switching | a kernel with the `asus-armoury` driver (`/sys/class/firmware-attributes/asus-armoury`), systemd, polkit |
| switching without a reboot | additionally `setpci` (`pciutils`), and a compositor that does not use the NVIDIA card (see below) |

supergfxd is **not** needed. If it is installed and cardwire is not, the tray offers its modes as
a fallback.

## Installation

```bash
git clone https://github.com/jkocon/Asus_GPU_Tray.git
cd Asus_GPU_Tray
sudo ./install.sh
```

The installer warns about missing dependencies. On ASUS laptops it also installs the hardware
switch backend: root-owned scripts, systemd units and a polkit rule. The tray starts at your next
login. To start it right away, run `python3 /usr/local/lib/asus-gpu-tray/asus_gpu_tray.py &`.

### One-time KDE Plasma setup for switching without a reboot

The NVIDIA driver can only let go of the card when no process has it open. The compositor normally
does, so KWin has to be kept on the iGPU. The setup has three parts:

1. **A stable name for the iGPU.** Use your iGPU's PCI address, shown as `iGPU:` in
   `python3 asus_gpu_tray.py --dump`:

   ```bash
   # /etc/udev/rules.d/70-asus-igpu-card.rules
   SUBSYSTEM=="drm", KERNEL=="card[0-9]*", KERNELS=="0000:3a:00.0", SYMLINK+="dri/asus-igpu-card"
   ```

2. **KWin uses only that GPU:**

   ```bash
   # ~/.config/plasma-workspace/env/asus-gpu-tray-kwin.sh
   [ -e /dev/dri/asus-igpu-card ] && export KWIN_DRM_DEVICES=/dev/dri/asus-igpu-card
   ```

3. **KWin does not load NVIDIA's EGL driver.** Without this, EGL still opens `/dev/nvidia0`. This
   applies to the compositor only; games still get the NVIDIA GPU:

   ```ini
   # ~/.config/systemd/user/plasma-kwin_wayland.service.d/asus-gpu-tray.conf
   [Service]
   Environment=__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json
   ```

Then run `sudo udevadm control --reload && sudo udevadm trigger -s drm` and log in again.

`KWIN_DRM_DEVICES` is colon-separated, so a `/dev/dri/by-path/pci-…` path cannot be used, which is
why the udev rule is needed. With this setup, monitors plugged into the XG Mobile's own ports stay
dark. On the GV601RE they did not work with the NVIDIA driver anyway. The laptop's panel and its
own ports keep working.

Without this setup everything else works, and the tray offers a reboot switch instead.

## Usage

Right-click the icon:

| Section | What it does |
|---|---|
| (top) | every GPU with its driver and power state (`active`, `suspended`, `blocked`), plus the XG Mobile dock status |
| **GPU access (live, cardwire)** | **Integrated** blocks the dGPU/eGPU for new apps. **Hybrid** allows all GPUs. **Smart** allows the dGPU only for approved apps. Apps that are already running keep their GPU. |
| **Hardware** | **Built-in dGPU** / **XG Mobile** switch live in about 40 s. **… only (MUX) – reboot** reboots right away and applies the mode during boot. XG Mobile is greyed out until the dock is connected and locked. |

Before a live switch the tray closes ROG Control Center, which keeps the card open, and starts it
again afterwards (in the tray). Close games and other apps that use the NVIDIA GPU first. If
something still holds the card, the switch is aborted without touching the hardware, and the tray
offers to switch with a reboot instead.

Left-click shows a notification with the active GPU. Hover for a summary.

### The icon

| Icon | Meaning |
|---|---|
| NVIDIA logo | an NVIDIA card is working (awake, driver bound, not blocked) |
| red **AMD** / blue **Intel** | the iGPU is doing the work |
| grey **GPU** / **?** | unknown vendor / no GPU found |
| purple dot | an external GPU is attached (XG Mobile mode or a Thunderbolt eGPU) |

### Command line

```bash
python3 asus_gpu_tray.py --dump                  # detected GPUs, modes and backends; no GUI
systemctl start asus-gpu-live@AsusEgpu.service   # live switch to the XG Mobile (…@Hybrid: built-in dGPU)
systemctl start asus-gpu-switch@AsusMuxDgpu.service   # schedule a mode and reboot now
```

Local administrators (group `wheel` or `sudo`) can start these units without a password.

## Troubleshooting

```bash
cat /var/lib/asus-gpu-tray/live-progress         # steps of the last live switch
journalctl -b -u 'asus-gpu-live@*'               # the same in the journal
journalctl -b -u asus-gpu-switch-apply           # what the boot-time switch did
cardwire get; cardwire list                      # cardwire mode and blocked GPUs
cat /sys/class/firmware-attributes/asus-armoury/attributes/{egpu_connected,egpu_enable,gpu_mux_mode}/current_value
```

| Message | Meaning |
|---|---|
| *the NVIDIA card is still in use by: …* | Close the listed apps. If `kwin_wayland`, `Xwayland` or `systemd-logind` are listed, the KDE setup above is missing or you have not logged in again since. |
| *The bus reset below … failed* / *cannot reset the bus* | Your kernel cannot reset the card from its port. Use the reboot switch. |
| *setpci (pciutils) is required* | Install `pciutils`. |
| *MUX mode is active* | In MUX mode the dGPU drives the panel. Switch with a reboot. |
| *Another GPU switch is already running* | Wait for it to finish. |

To cancel a scheduled reboot switch before rebooting, run
`sudo rm /var/lib/asus-gpu-tray/pending /etc/modprobe.d/zz-asus-gpu-tray-switch.conf`.

## Known issues

- **Black screen for about 35 s during a reboot switch.** The firmware call behind `egpu_enable`
  takes about 30 s, and the login screen waits so it cannot grab the card mid-switch.
- **Extra power cycle after a reboot switch** from the XG Mobile to the built-in dGPU. This was seen
  once, with supergfxd disabled. The laptop restarted by itself during POST (Linux logged a normal
  reboot), and the second boot came up fine. The cause is unknown.
- **cardwire may miss the dGPU at boot.** It can start before the NVIDIA driver is ready and treat
  the laptop as a desktop, offering only Hybrid and Manual. The tray detects this and runs
  `cardwire debug refresh-gpu`.

## Limitations

- The live switch has been tested on two laptops (GV601RE, GV301QE) and one dock only. It handles exactly one NVIDIA
  GPU (dGPU or XG Mobile) behind one PCIe port.
- If NVIDIA modules are loaded from the initramfs (early KMS), the boot-time switch finds the driver
  already bound and refuses to run. The live switch is not affected.
- Undock only in built-in dGPU mode. Unplugging an active XG Mobile is a surprise removal of a GPU
  that is in use.
- GPU classification uses heuristics (see [how it works](docs/how-it-works.md#gpu-detection)).
  Thunderbolt eGPUs and Intel laptops are untested.

## Uninstall

```bash
sudo ./uninstall.sh
```

This removes everything `install.sh` installed and any scheduled switch. It does not remove the
KDE setup files, which you created yourself.

## Development

```bash
QT_QPA_PLATFORM=offscreen python3 -m unittest discover -s tests   # no ASUS hardware needed
ruff check --line-length 120 asus_gpu_tray.py tests/
shellcheck scripts/* install.sh uninstall.sh
```

| Path | Contents |
|---|---|
| `asus_gpu_tray.py` | the tray (runs as the user) |
| `scripts/asus-gpu-switch-live` | live switch (root, `asus-gpu-live@.service`) |
| `scripts/asus-gpu-switch-reboot` | schedules a mode and reboots (root, `asus-gpu-switch@.service`) |
| `scripts/asus-gpu-switch-apply` | applies a scheduled mode at boot (root, `asus-gpu-switch-apply.service`) |
| `systemd/`, `polkit/`, `desktop/`, `icons/` | units, the polkit rule, launcher and autostart entries, icons |
| `tests/` | unit tests |

Bug reports and hardware reports are welcome. Please include `--dump` output, your laptop model and
`/var/lib/asus-gpu-tray/live-progress` for switch problems. For security issues, see
[SECURITY.md](SECURITY.md).

## License

GPL-3.0-or-later, see [LICENSE](LICENSE). The NVIDIA logo shape in `icons/nvidia.svg` comes from the
[char-white](https://github.com/CachyOS/char-white) icon theme (GPL).

Not affiliated with ASUS, NVIDIA or the Open Gaming Collective. Switching GPU hardware from software
relies on firmware behaviour that ASUS does not document for Linux. Save your work before switching.

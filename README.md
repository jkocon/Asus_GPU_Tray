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
- **Who woke the dGPU.** On battery, a notification names the apps that woke the built-in dGPU.
- **GPU numbers.** Temperature, power, load and memory of an awake NVIDIA GPU in the menu.
- **Dock prompt.** Locking the XG Mobile on the built-in dGPU offers the switch to it, no reboot.
- **Undock now.** One click closes every app that uses the XG Mobile and switches to the built-in
  dGPU, so the dock can be unplugged.
- **Undock check.** If the XG Mobile is unlocked while it is the active GPU, the tray brings the
  built-in dGPU back without a reboot when nothing used the GPU, and otherwise offers the reboot.
- **Lost GPU detection.** A GPU that fell off the PCIe bus is reported, with a reboot offer. While
  the XG Mobile is active it is kept out of D3cold, which is what made it fall off.

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

With Plasma Login Manager, `install.sh` also installs a systemd user environment generator
(`/etc/systemd/user-environment-generators/60-asus-gpu-tray-greeter`) that applies the same two
settings to the login screen once `/dev/dri/asus-igpu-card` exists. Without it, a NVIDIA driver
that hangs at boot also hangs the login screen on a black screen.

`KWIN_DRM_DEVICES` is colon-separated, so a `/dev/dri/by-path/pci-…` path cannot be used, which is
why the udev rule is needed. With this setup, monitors plugged into the XG Mobile's own ports stay
dark. On the GV601RE they did not work with the NVIDIA driver anyway. The laptop's panel and its
own ports keep working.

Without this setup everything else works, and the tray offers a reboot switch instead.

## Usage

Right-click the icon:

| Section | What it does |
|---|---|
| (top) | every GPU with its driver and power state (`active`, `suspended`, `blocked`), plus the XG Mobile dock status. An NVIDIA GPU that is awake anyway also shows temperature, power, load and memory (from `nvidia-smi`, read only when the menu opens, so a sleeping card is never woken and an awake one is not kept awake) |
| **Notify when the dGPU wakes up on battery** | on by default: when the built-in dGPU has been awake for a few seconds on battery, a notification names the apps that use it (each set of apps at most every 10 minutes) |
| **GPU access (live, cardwire)** | **Integrated** blocks the dGPU/eGPU for new apps. **Hybrid** allows all GPUs. **Smart** allows the dGPU only for approved apps. Apps that are already running keep their GPU. |
| **Undock now (close all GPU apps)…** | XG Mobile mode only: after one confirmation it closes every app that has the GPU open (SIGTERM, SIGKILL after 5 s) and switches to the built-in dGPU live. |
| **Hardware** | **Built-in dGPU** / **XG Mobile** switch live in about 40 s. **… only (MUX) – reboot** reboots right away and applies the mode during boot. XG Mobile is greyed out until the dock is connected and locked. |

Before a live switch the tray closes ROG Control Center, which keeps the card open, and starts it
again afterwards (in the tray). Other apps of yours that have the NVIDIA GPU open are listed, and
the tray asks whether to kill them. If you say no, it offers to reboot and switch during boot
instead. Parts of the desktop session (`kwin_wayland`, `Xwayland`, `plasmashell`, …) are never
killed; if one of them holds the card, only the reboot is offered. If something the tray cannot
see (another user, a system service) still holds the card, the switch is aborted without touching
the hardware, and the tray asks again or offers the reboot.

While a live switch runs, a *Switching in progress* window shows what is happening (preparing,
disconnecting the old GPU, the firmware switch, connecting the new GPU) and the elapsed time, like
the window of Armoury Crate on Windows. It cannot be closed until the switch is over. When it
succeeds, the window says so and closes after 10 seconds; when it fails, a dialog explains why.

### Docking

When you connect and lock the XG Mobile while the built-in dGPU is active, the tray asks whether
to switch to it now, without a reboot (the same live switch as from the menu). It asks once per
lock, and not when the tray starts with the dock already locked.

### Undocking

**Switch to the built-in dGPU first, then unlock the XG Mobile.** In a hurry, use **Undock now
(close all GPU apps)…**: one confirmation lists the apps that use the XG Mobile, closes them all
without further questions (unsaved work in them is lost), stops the GPU services and switches to
the built-in dGPU; unplug when it says *You can disconnect the XG Mobile now*. This only works
before unlocking: once the lock is open, closing the apps would hang in the NVIDIA driver.

Opening the lock switch on the
cable while the XG Mobile is the active GPU makes the firmware drop the GPU at once (on a GV601RE
about 0.1 s after the lock event). There is no time to switch first. Apps that were using it may
stop responding, and the GPU does not come back when you connect and lock the dock again. Only a
reboot recovers it.

When that happens, the tray shows *XG Mobile: disconnected while in use – reboot needed*, adds a
red dot to the icon and offers an emergency reboot that starts on the built-in dGPU. It never
tries a live switch then: the firmware can take up to about a second to remove the GPU, and a
live switch started in that window hung in the kernel. The live switch script refuses to run
while the XG Mobile is unlocked or after a GPU loss in the current boot.

In XG Mobile mode the menu labels the built-in dGPU *– before undocking*, and the window and
notification after a switch to the XG Mobile remind you to switch back before unlocking.

**Unlocking without a reboot** works when nothing holds the XG Mobile's GPU at that moment. The
driver then lets it go, the firmware switches back to the built-in dGPU by itself (as on Windows),
and the tray brings the dGPU back without asking (link up and PCI rescan, a few seconds). To get
there, close games, browsers and ROG Control Center, stop `cardwired` and `nvidia-powerd`, and
check with `sudo python3 asus_gpu_tray.py --holders` that it prints *Nobody holds the NVIDIA
card*. If something still held the GPU, the firmware does not switch back, and after a few seconds
the tray offers the reboot. Switching to the built-in dGPU from the menu first is still the
simple way.

### Lost GPU

When the kernel reports that an NVIDIA GPU fell off the bus (Xid 79, or *Unable to change power
state from D3cold to D0*), the tray asks once whether to reboot, adds a red dot to the icon and a
*… fell off the bus – Reboot…* item to the menu. The card cannot come back without a reboot. If
the XG Mobile is unlocked at that point, the reboot also switches to the built-in dGPU. Reading
the kernel log needs access to the system journal (groups `wheel`, `adm` or `systemd-journal`);
without it, only a runtime PM error in sysfs is detected.

While the XG Mobile is the active GPU, a udev rule sets `d3cold_allowed = 0` on its PCI functions.
The card can still sleep in D3hot, and the dock has its own power supply. Waking the XG Mobile from
D3cold once left its RTX 3070 lost until a reboot. `--dump` shows the current setting.

Left-click shows a notification with the active GPU (and its numbers when it is an awake NVIDIA
GPU). Hover for a summary. Settings are stored in `~/.config/asus-gpu-tray/asus-gpu-tray.conf`.

### The icon

| Icon | Meaning |
|---|---|
| NVIDIA logo | an NVIDIA card is working (awake, driver bound, not blocked) |
| red **AMD** / blue **Intel** | the iGPU is doing the work |
| grey **GPU** / **?** | unknown vendor / no GPU found |
| purple dot | an external GPU is attached (XG Mobile mode or a Thunderbolt eGPU) |
| red dot | a GPU fell off the bus, or the XG Mobile is unlocked while still in use |

### Command line

```bash
python3 asus_gpu_tray.py --dump                  # detected GPUs, modes and backends; no GUI
sudo python3 asus_gpu_tray.py --holders         # processes that have the NVIDIA card open
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
| *the NVIDIA card is still in use by: …* | The tray lists your own apps and offers to kill them. If `kwin_wayland`, `Xwayland` or `systemd-logind` are listed, the KDE setup above is missing or you have not logged in again since. |
| *… fell off the bus* | The GPU stopped responding (often while waking from sleep). Reboot. `journalctl -k -b --grep 'fallen off the bus|D3cold to D0'` shows the kernel messages. |
| *The bus reset below … failed* / *cannot reset the bus* | Your kernel cannot reset the card from its port. Use the reboot switch. |
| *setpci (pciutils) is required* | Install `pciutils`. |
| *MUX mode is active* | In MUX mode the dGPU drives the panel. Switch with a reboot. |
| *Another GPU switch is already running* | Wait for it to finish. |

To cancel a scheduled reboot switch before rebooting, run
`sudo rm /var/lib/asus-gpu-tray/pending /etc/modprobe.d/zz-asus-gpu-tray-switch.conf`.

## Known issues

- **About 40 s before the login screen during a reboot switch.** The firmware call behind
  `egpu_enable` takes about 30 s, and the login screen waits so it cannot grab the card
  mid-switch. The text console shows what is happening meanwhile.
- **Extra power cycle after a reboot switch** from the XG Mobile to the built-in dGPU. This was seen
  once, with supergfxd disabled. The laptop restarted by itself during POST (Linux logged a normal
  reboot), and the second boot came up fine. The cause is unknown.
- **After a GPU loss a normal shutdown hangs.** The NVIDIA driver wedges once it has lost a GPU
  (`nvidia-modeset: Error while waiting for GPU progress` every 5 s, a warning in `nvidia_close`),
  and processes that close the device hang in the kernel. Seen twice on a GV601RE. That is why the
  tray's reboot after a GPU loss is an emergency reboot (SysRq: sync, remount read-only, reset):
  open apps are not asked to quit. If you reboot some other way and it hangs, Alt+SysRq+S, U, B
  does the same by hand (if the keyboard SysRq is enabled).
- **Two restarts after unlocking an active XG Mobile.** After the emergency reboot the laptop
  starts in XG Mobile mode without a locked dock. The ASUS firmware then switches back to the
  built-in dGPU by itself and resets once more: the bootloader appears, the screen goes black,
  and the laptop restarts on its own. The second start is normal and needs nothing from you. The
  tray could avoid it only by switching the firmware before the reboot, which with a lost GPU
  risks an instant reset, so it does not.
- **cardwire may miss the dGPU at boot.** It can start before the NVIDIA driver is ready and treat
  the laptop as a desktop, offering only Hybrid and Manual. The tray detects this and runs
  `cardwire debug refresh-gpu`; when that does not help a minute later, it restarts cardwired (at
  most twice; the polkit rule allows that one restart).

## Limitations

- The live switch has been tested on two laptops (GV601RE, GV301QE) and one dock only. It handles exactly one NVIDIA
  GPU (dGPU or XG Mobile) behind one PCIe port.
- If NVIDIA modules are loaded from the initramfs (early KMS), the boot-time switch finds the driver
  already bound and refuses to run. The live switch is not affected.
- Undock only in built-in dGPU mode. Unlocking an active XG Mobile is a surprise removal of a GPU
  that is in use: the NVIDIA driver keeps the dead device (*Attempting to remove device … with
  non-zero usage count*), and only a reboot recovers it. The tray cannot prevent this; it can only
  report it.
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

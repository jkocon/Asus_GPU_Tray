# How it works

This document explains the design of Asus GPU Tray and how the XG Mobile switch was made to work
on Linux, including the approaches that failed. Everything was measured on an ASUS ROG Flow X16
GV601RE (Radeon 680M + RTX 3050 Ti) with an XG Mobile (RTX 3070), running nvidia-open 615 and
kernel 7.2.

## Components

```
 user session                        root (systemd units, started via polkit)
┌───────────────────────────┐        ┌────────────────────────────────────────────────┐
│ asus_gpu_tray.py          │ start  │ asus-gpu-live@<mode>.service                   │
│  - reads sysfs, cardwire  │───────▶│   scripts/asus-gpu-switch-live   (live switch) │
│  - draws icon and menu    │        │ asus-gpu-switch@<mode>.service                 │
│  - closes/restarts RCC    │───────▶│   scripts/asus-gpu-switch-reboot (schedule +   │
└─────────────┬─────────────┘        │                                   reboot)      │
              │ cardwire set         │ asus-gpu-switch-apply.service   (at boot)      │
              ▼                      │   scripts/asus-gpu-switch-apply                │
        cardwired (eBPF)             └────────────────────────────────────────────────┘
```

The tray never needs root. It reads kernel-cached sysfs files and cardwire's device list, and it
changes state only through `cardwire set` and by starting the switch units. A polkit rule lets
local administrators start those units without a password (see [SECURITY.md](../SECURITY.md)).

## GPU detection

The tray reads `/sys/bus/pci/devices/*` for display controllers (class `0x03xxxx`) and takes
their names from `/usr/share/hwdata/pci.ids`. It deliberately avoids `lspci`, `nvidia-smi` and
opening `/dev/nvidia*`, because those wake a runtime-suspended dGPU.

| Kind | Rule |
|---|---|
| iGPU | an Intel card on PCI bus 0, or an AMD card whose bus also holds USB controllers (an APU) |
| eGPU | an ancestor port has `external_facing = 1` or the device is `removable` (Thunderbolt / USB4), or it is an NVIDIA card while ASUS `egpu_enable = 1` (XG Mobile uses the same port as the built-in dGPU) |
| dGPU | anything else |

cardwire hides the sysfs files of a blocked GPU from every process, root included. Blocked cards
are therefore taken from `cardwire list --json`.

The icon shows the card that is actually working: a dGPU/eGPU that is awake (`runtime_status`
active), has a driver and is not blocked. Otherwise it shows the iGPU, or the dGPU in MUX mode,
where the dGPU drives the panel.

## GPU access modes (cardwire)

[cardwire](https://github.com/OpenGamingCollective/cardwire) blocks access to a GPU's device
nodes with eBPF LSM hooks. It never unloads a driver, so it switches instantly. The tray runs
`cardwire get` and `cardwire set <mode>`.

cardwired can start before the NVIDIA driver has created its DRM nodes. In that case its
discrete-GPU check fails and it treats the laptop as a desktop (only `hybrid`/`manual`). When the
tray sees that cardwire lists no discrete GPU while sysfs shows one with a driver, it runs
`cardwire debug refresh-gpu` (at most once a minute).

## Hardware modes (ASUS firmware)

The `asus-armoury` driver exposes firmware settings under
`/sys/class/firmware-attributes/asus-armoury/attributes/`:

| Attribute | Meaning |
|---|---|
| `egpu_connected` | 1 when the XG Mobile is plugged in and its lock switch is closed |
| `egpu_enable` | 1 routes the PCIe lanes to the XG Mobile, 0 to the built-in dGPU; persists across reboots |
| `gpu_mux_mode` | 0 means the dGPU drives the panel (MUX), 1 means hybrid; applied by the firmware at the next boot |
| `dgpu_disable` | powers the built-in dGPU off |

Writing `egpu_enable` makes the firmware move the lanes. The call takes about 30 seconds. The old
card disappears from the bus, and the new one appears at the same address (`0000:01:00.0`).

## Live switch: built-in dGPU ⇄ XG Mobile

`scripts/asus-gpu-switch-live` does the following. The first three steps change nothing on the
hardware. Every step is appended to `/var/lib/asus-gpu-tray/live-progress` followed by `sync`,
so after a hard hang the file shows where it stopped.

1. **Checks.** The mode is valid; the dock is connected (for XG Mobile); no other switch is running
   (`flock` on `/run/asus-gpu-tray.lock`); no reboot switch is pending; the MUX is off; `setpci`
   exists.
2. **Stop services** that keep the card open: cardwired, nvidia-powerd, nvidia-persistenced,
   supergfxd. Stopping cardwired also lifts its blocks, so a GPU blocked in Integrated/Smart mode
   becomes visible in sysfs again. cardwired restores its mode when it is started afterwards.
3. **Find the card and its port.** All NVIDIA functions (GPU and HDMI audio) must sit behind one
   port, and the kernel must be able to reset the bus below it (`reset_subordinate`).
4. **Keep everyone away.** The script sets `chmod 000` on `/dev/nvidia*`, `/dev/nvidia-caps/*` and
   the card's DRM nodes, so nothing new can open them. It then scans `/proc/*/fd` of every process.
   If anything still holds one of the nodes, it aborts and lists the holders. This matters: the
   NVIDIA driver waits **forever** in its PCI remove callback while the card is open, which hangs
   the machine.
5. **Unbind** the NVIDIA driver and `snd_hda_intel`.
6. **Secondary bus reset** below the root port (`reset_subordinate`). If this fails, the switch is
   aborted before the firmware is touched.
7. **Remove** the functions from the PCI core.
8. **Link down/up cycle**: set Link Disable on the root port. pciehp notices ("Link Down",
   "Card present") and turns the link back on within a second. On ports with AER, Surprise Down
   is masked first; on the GV601RE that write has no effect (see below).
9. **Firmware switch**: write `egpu_enable` (and `dgpu_disable=0` when going to the built-in
   dGPU), then read it back.
10. **Bring the port back**: clear Link Disable, wait up to 20 s for the link (the XG Mobile cable
    needs about 8 s), clear the AER status and restore the mask.
11. **Rescan PCI.** The still-loaded NVIDIA driver binds to the new card.
12. **Restore** the device node permissions, start the stopped services, refresh cardwire and
    report the result.

Any failure, and SIGTERM (for example the unit's 5-minute timeout), goes through the same exit
path. That path turns the link back on, rescans or re-probes the card, restores the permissions
and restarts the services.

The tray adds two things around this. It closes ROG Control Center, which keeps `/dev/nvidia0`
open, and starts it again afterwards with `--background`. It also offers the reboot switch if
the live switch is aborted.

### How it got there

| Attempt | Result |
|---|---|
| supergfxd's switch (log out, `rmmod nvidia`, switch, reload) | hard hang; the last log line is `nvidia-modeset: Unloading` |
| unbind + remove + write `egpu_enable` with KWin still using the card | refused by the holder check |
| same, KWin kept on the iGPU (`KWIN_DRM_DEVICES`) | KWin still held `/dev/nvidia0` through glvnd's NVIDIA EGL driver; fixed with `__EGL_VENDOR_LIBRARY_FILENAMES` for KWin only |
| unbind + remove + write `egpu_enable`, nothing holding the card | **instant reset** in both directions; next boot: `Previous system reset reason [0x08000800]: an uncorrected error caused a data fabric sync flood event` |
| same + FLR of the GPU (a bus reset from the GPU's side was rejected) | instant reset |
| unbind + **secondary bus reset from the root port** + remove + **link down/up cycle** + write `egpu_enable` | **works**, every time in both directions |

An earlier version of this project said that masking the root port's "Surprise Down" AER error
was the fix. That was wrong. The GV601RE's root port cannot report Surprise Down at all (`LnkCap:
Surprise-`), and writes to that mask bit have no effect. What changed between the failing and the
working attempts is the secondary bus reset, and the link down/up cycle that the Link Disable write
triggers through pciehp. Separating the two would have taken more attempts, each of which resets
the machine when it fails, so the script does both.

The most likely explanation is that the firmware's lane switch must not happen while the GPU is
still in the state the driver left it in. The boot-time switch works without either step because
the driver has not touched the card at that point.

## Reboot switch (and MUX)

Used for the MUX mode, and as the fallback when a live switch cannot run.

```
menu click
  └─ asus-gpu-switch@<Mode>.service → scripts/asus-gpu-switch-reboot
       ├─ writes gpu_mux_mode if needed (the firmware applies it at the next boot)
       ├─ /var/lib/asus-gpu-tray/pending = <Mode>
       ├─ /etc/modprobe.d/zz-asus-gpu-tray-switch.conf blacklists nvidia for one boot
       └─ systemctl reboot
boot
  └─ asus-gpu-switch-apply.service → scripts/asus-gpu-switch-apply
       ├─ runs before cardwired, nvidia-powerd, nvidia-persistenced, supergfxd and the display
       │  manager (nvidia-modprobe loads the module explicitly, past the blacklist)
       ├─ removes the NVIDIA functions, writes egpu_enable, rescans PCI
       ├─ updates supergfxd's config if supergfxd is installed
       └─ always removes the blacklist and loads the driver (supergfxd or a udev "add" replay)
```

The unit also runs when only the blacklist is left behind, so the NVIDIA driver is never blocked
for good. The display manager waits for it, which is the ~35 s black screen during a reboot switch.

## Files and state

| Path | Written by | Contents |
|---|---|---|
| `/var/lib/asus-gpu-tray/pending` | reboot script | mode to apply at the next boot |
| `/var/lib/asus-gpu-tray/live-progress` | live script | steps of the last live switch |
| `/var/lib/asus-gpu-tray/live-result` | live script | result message for the tray |
| `/etc/modprobe.d/zz-asus-gpu-tray-switch.conf` | reboot script | one-boot NVIDIA blacklist |
| `/run/asus-gpu-tray.lock` | live and reboot scripts | serializes switches |
| `$XDG_RUNTIME_DIR/asus-gpu-tray.lock` | tray | single instance per session |

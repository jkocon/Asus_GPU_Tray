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

The tray adds a few things around this:

- It closes ROG Control Center, which keeps `/dev/nvidia0` open, and starts it again afterwards
  with `--background`.
- It shows a *Switching in progress* window and maps the last line of `live-progress` to a stage.
  The window is a plain raster Qt widget: showing it does not load an EGL/GL driver or open any
  `/dev/nvidia*` or `/dev/dri/*` node (checked in `/proc/self/fd`), so the tray itself never
  becomes a holder of the card.
- **Undock now** (XG Mobile mode only) is the same live switch to the built-in dGPU with a single
  confirmation: it lists the holders once, closes them all without asking again, and refuses
  (offering the reboot) when a desktop session process holds the card or the dock is already
  unlocked.
- It runs the same holder check for the user's own processes first (reading `/proc/<pid>/fd`
  does not wake the card). It lists them and offers to kill them (SIGTERM, then SIGKILL after
  5 s). If the user declines, or a session process such as `kwin_wayland` holds the card, it offers
  the reboot switch instead.
- If the script still aborts because of holders (something opened the card in between), the tray
  asks again, up to twice, and then offers the reboot switch.
- When a GPU is lost (below), it does not attempt a live switch at all and offers only the reboot
  switch: unbinding the driver from a card that no longer answers is untested.

## Unlocking an active XG Mobile

The first design assumed that the lock switch would warn before the GPU goes away, leaving time for
a live switch. A test on the GV601RE showed otherwise:

```
21:28:19.320 asus_wmi: Unknown key code 0xba          lock switch opened
21:28:19.320 asus_wmi: Unknown key code 0xbe          eGPU state change
21:28:19.438 [nvidia-drm] Removing device             the firmware has already dropped the GPU
21:28:19.490 NVRM: Attempting to remove device 0000:01:00.0 with non-zero usage count!
21:28:39.544 asus_wmi: Unknown key code 0xbc          dock connected again
21:28:41.467 asus_wmi: Unknown key code 0xba          lock switch closed - no "Card present" follows
```

The lock event (0xba) also shows up when the lock is moved in built-in dGPU mode, without anything
else happening. In XG Mobile mode the firmware follows it with 0xbe within a millisecond and
removes the GPU. pciehp logs no Link Down. The NVIDIA driver keeps the device it could not tear
down, the apps that had it open keep stale handles, and the slot stays empty after the dock is
locked again.

A later test with nothing holding the GPU (games, browsers and ROG Control Center closed,
cardwired and nvidia-powerd stopped) went differently. The driver let the GPU go ("Removing
device", no usage-count warning), and the firmware switched `egpu_enable` back to 0 by itself
within a second (key code 0xc2, as after our own firmware calls). That is the Windows behaviour:
the OS releases the device, the firmware completes the switch. It left the root port with Link
Disable set and the built-in dGPU off the bus. `asus-gpu-switch-live Hybrid` handles that case
("already in Hybrid, but no NVIDIA device"): it clears Link Disable, waits for the link, rescans,
and the still-loaded driver binds to the dGPU. So the firmware's reaction tells the two cases
apart: switched back by itself means clean (bring the dGPU back live), `egpu_enable` still 1 means
the driver held on and is wedged (reboot). The tray waits up to 5 s after the unlock to see which.

A repeat on 2026-10-09 (NVIDIA 615.71, cardwired and nvidia-powerd stopped, nothing holding the
card as root) did not go that way. The driver logged "Removing device" without the usage-count
warning, but the firmware left `egpu_enable` at 1 and the GPU stayed on the bus without a driver.
The kernel stacks showed a deadlock: the ACPI eject (`acpiphp_disable_and_eject_slot` →
`nv_pci_remove` → `nv_acpi_methods_uninit`) waited for the ACPI notify queue, whose worker was stuck
in `nv_acpi_powersource_hotplug_event` waiting for the RM lock held by the remove path (the unlock
also reports a power source change). Any later PCI remove or rescan blocks behind it, and only a
SysRq reboot gets out. What differed from the successful 2026-10-04 test is not known; one
candidate is the power source event (the XG Mobile was charging the laptop, with no other charger
plugged in). So a clean unlock cannot be relied on: switch to the built-in dGPU first.
(Stacks: `rust/tests/reference/x16/07b-clean-unlock-services-stopped/hung-stacks.txt` on the
`rust` branch.)

So the tray treats "XG Mobile mode, but no NVIDIA GPU on the bus" (`xg_gone`) as the real signal.
It checks `/sys/bus/pci/devices/<addr>`, because cardwire keeps listing a blocked GPU after it has
been removed. On the change into that state it shows a notification and offers the reboot switch to
the built-in dGPU, and the menu keeps a *Reboot…* item. It never tries a live switch, also not when the
GPU is still on the bus right after the lock event: in a second test the firmware took 1.1 s to
remove it, the tray caught that window and started a live switch, and the kernel's removal of the
GPU hung inside the NVIDIA driver while holding the PCI rescan lock. The live script then blocked
for good in `echo 1 > /sys/bus/pci/rescan` (state D), still holding the switch lock. Now the tray
treats "unlocked while active" like "gone", and the live script itself refuses to run while the
XG Mobile is unlocked in XG Mobile mode or when this boot's kernel log shows a GPU loss.

The reboot itself is the next problem. After the loss the NVIDIA driver is wedged: nvidia-modeset
logs `Error while waiting for GPU progress` every 5 s, closing the device triggers a warning in
`nvidia_dev_put`, and the processes doing so hang in the kernel. A normal shutdown waited for them
until the laptop was powered off by hand, twice. So `asus-gpu-switch-reboot` checks whether a GPU
was lost (XG Mobile mode without an NVIDIA device on the bus, or a kernel message from this boot
matching `(NVRM|nvidia).*(fallen off the bus|with non-zero usage count|D3cold to D0)`). If so, it
schedules the mode as usual and then reboots through SysRq: `s` (sync), `u` (remount read-only),
`b` (reset). It also goes ahead when a hung live switch still holds the switch lock, but only
after a GPU loss. The tray routes its "GPU lost – reboot" through this unit, keeping the current
hardware mode, or the built-in dGPU when the XG Mobile was unlocked. If
the lock opens and the GPU is still there (not seen so far), the tray starts the live switch to the
built-in dGPU without asking; only an explicit `egpu_connected = 0` counts, never a failed read.

## Lost GPUs and D3cold

On 2026-10-01 the XG Mobile's RTX 3070 fell off the bus while runtime-suspended, most likely when
something woke it: `Unable to change power state from D3cold to D0, device inaccessible`, Xid 79,
and the root port retraining a "broken device". Vulkan then listed only the iGPU until a reboot.

Two measures:

- **Prevention.** `udev/72-asus-gpu-tray-egpu.rules` runs `scripts/asus-gpu-egpu-power` when an
  NVIDIA PCI function is added or bound, and when the asus-armoury attributes appear (at boot the
  two can come in either order). When `egpu_enable` is 1, the script sets `d3cold_allowed = 0` on
  every NVIDIA function. The kernel then limits the card to D3hot and keeps its root port powered.
  The live switch also runs the script directly before restarting cardwired, because cardwired
  hides a blocked card's sysfs files from root as well. The built-in dGPU keeps D3cold: its PCI
  functions are created anew on every switch, with the kernel default.
- **Detection.** The tray follows `journalctl -k -b -f --grep 'fallen off the bus|Unable to change
  power state from D3cold to D0'` and maps each message to a PCI slot. A message counts if it is
  newer than the last successful live switch (which re-creates the devices). A `runtime_status` of
  `error` in sysfs counts too. The tray asks once per card whether to reboot.

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
       └─ systemctl reboot (SysRq emergency reboot after a GPU loss)
boot
  └─ asus-gpu-switch-apply.service → scripts/asus-gpu-switch-apply
       ├─ runs before cardwired, nvidia-powerd, nvidia-persistenced, supergfxd and the display
       │  manager
       ├─ egpu_enable already right (MUX only, or the firmware switched back by itself): done
       ├─ loads the NVIDIA driver on the current card, then runs scripts/asus-gpu-switch-live
       ├─ no NVIDIA card visible: fallback, direct firmware switch (see below)
       └─ updates supergfxd's config if supergfxd is installed
```

The unit also runs when only a blacklist from an older version is left behind, and removes it.

In MUX mode the panel is wired to the dGPU, so KWin must not be kept on the iGPU: on 2026-10-09
the first MUX boot with `KWIN_DRM_DEVICES` pointing at the iGPU showed only a black screen (KWin:
"There are no outputs"). The KWin settings (`kde/asus-gpu-tray-kwin.sh`, `kde/asus-gpu-tray.conf`
and the login screen's generator) check `gpu_mux_mode` and stay out of the way when it is 0.

**Current design (2026-10-04, later):** the boot-time switch no longer blacklists NVIDIA. The
driver comes up on the current card as on any boot, and `asus-gpu-switch-apply` runs
`asus-gpu-switch-live` before the login screen starts. It stops the boot splash first (it went
black while the GPUs changed) and writes what is happening, including each step of the switch, to
the text console on `/dev/tty1`. The reason for this design: loading the driver fresh right after the firmware switch
deadlocked inside it on 2 of 3 boots. Kernel stacks showed the GSP init (`kgspInitRm`) and two ACPI
notify workers (`rm_acpi_nvpcf_notify`, the NVPCF notifications the firmware sends after the
switch) all waiting for the RM API lock, and every later module load (sound, Bluetooth) stuck
behind the unfinished probe. In many live switches the already running driver never hit this,
also with the probe starting 0.4 s after the firmware call. If the live switch fails at boot, the
laptop stays in its current mode. The direct path below is only a fallback when no NVIDIA card is
visible at all.

Before that, the boot-time switch used the same reset and link cycle as the live switch:
secondary bus reset below the root port, remove the functions, Link Disable, `egpu_enable`, Link
Enable and wait for the link, rescan. Before that it only removed the functions, wrote
`egpu_enable` and rescanned. After a warm reset the NVIDIA driver then hung once while probing
the built-in dGPU (`nvidia 0000:01:00.0: enabling device`, then nothing), and the login screen's
KWin hung with it. The root port is remembered in `/var/lib/asus-gpu-tray/root-port` for boots
where no NVIDIA card is visible (XG Mobile mode with the dock unplugged).

When the unit is done, it removes the one-boot blacklist, runs `udevadm control --reload`, replays
the NVIDIA "add" events and starts `modprobe@nvidia_drm.service` without waiting for it. Without the
reload, udevd still used the modprobe config it had read with the blacklist in place. When the unit
finished within milliseconds (nothing to switch), nvidia-powerd then loaded `nvidia` without its
softdeps, and `nvidia_drm` was missing for the whole boot (no DRM node, cardwire offered only
Hybrid/Manual). If the laptop starts in XG Mobile mode without a locked dock, the firmware switches
back to the built-in dGPU and resets by itself before Linux runs this unit. The display manager waits for it, which is the ~35 s black screen during a reboot switch.

## Files and state

| Path | Written by | Contents |
|---|---|---|
| `/var/lib/asus-gpu-tray/pending` | reboot script | mode to apply at the next boot |
| `/var/lib/asus-gpu-tray/live-progress` | live script | steps of the last live switch |
| `/var/lib/asus-gpu-tray/live-result` | live script | result message for the tray |
| `/etc/modprobe.d/zz-asus-gpu-tray-switch.conf` | older versions of the reboot script | one-boot NVIDIA blacklist, removed at boot |
| `/sys/bus/pci/devices/<NVIDIA>/d3cold_allowed` | `asus-gpu-egpu-power` (udev, live script) | 0 while the XG Mobile is active |
| `/run/asus-gpu-tray.lock` | live and reboot scripts | serializes switches |
| `$XDG_RUNTIME_DIR/asus-gpu-tray.lock` | tray | single instance per session |

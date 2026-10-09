# Security

## What runs with which privileges

| Part | Runs as | Can do |
|---|---|---|
| `asus-gpu-tray` (no subcommand) | the logged-in user | read sysfs, `/proc` and the kernel log, run `cardwire`, start the switch units, stop and restart the user's own ROG Control Center, kill the user's own processes that hold the NVIDIA card (only after the user confirms), reboot (logind) |
| `asus-gpu-tray switch-live` | root, `asus-gpu-live@<mode>.service` | stop and start GPU services, change device node permissions for the duration of the switch, unbind/reset/remove PCI devices, write ASUS firmware attributes |
| `asus-gpu-tray switch-reboot` | root, `asus-gpu-switch@<mode>.service` | write `gpu_mux_mode`, schedule a mode, reboot (an emergency SysRq reboot when the NVIDIA driver has lost a GPU) |
| `asus-gpu-tray switch-apply` | root, at boot | apply the scheduled mode |
| `asus-gpu-tray egpu-power` | root, from udev (`switch-live` does the same itself) | set `d3cold_allowed = 0` on NVIDIA functions while `egpu_enable` is 1; takes no input |

## The polkit rule

`/etc/polkit-1/rules.d/50-asus-gpu-tray.rules` allows starting these units without a password:

- `asus-gpu-switch@{Integrated,Hybrid,AsusEgpu,AsusMuxDgpu}.service`
- `asus-gpu-live@{Hybrid,AsusEgpu}.service`

and restarting `cardwired.service` (the tray does that when cardwired missed the dGPU at boot and
`cardwire debug refresh-gpu` did not help).

It allows only the `start` verb for the switch units and only `restart` for cardwired, and only for a subject that is **local**, **active** and in the
`wheel` or `sudo` group, the same people who can already use `sudo`. The rule does not grant
anything beyond what they have; it only removes the password prompt.

The consequence is that any program running in an administrator's active session can reboot the
machine or trigger a ~40 s live switch without asking. This is a nuisance, not an escalation,
but remove the rule if that matters to you. The tray then asks for the password through the
normal polkit agent.

## Input handling

- The unit instance (`%i`) is the only input to the root helpers. The polkit rule restricts it to
  the names above, and each helper validates it again. The mode read back from the `pending` file
  at boot is validated too.
- Nothing uses a shell. The helpers run a fixed set of system programs (`systemctl`, `setpci`,
  `udevadm`, `modprobe`, `journalctl`, `cardwire`, `plymouth`, `logger`) with fixed arguments, by
  name from the unit's `PATH`.
- The tray and the root helpers are the same binary. It is installed root-owned, mode 0755, in a
  root-owned directory (`/usr/local/lib/asus-gpu-tray` or `/usr/lib/asus-gpu-tray`); `install.sh`
  builds it as the invoking user and then installs a root-owned copy, so the user who built it
  cannot change what root runs.
- The switch lock is `/run/asus-gpu-tray.lock` in the root-only `/run`, not the world-writable
  `/run/lock` of some distributions, so other users cannot plant or hold it.
- The tray's single-instance lock lives in `$XDG_RUNTIME_DIR`, or falls back to a private
  `~/.cache/asus-gpu-tray` (0700). It is opened with `O_NOFOLLOW` and never truncated.
- Before a live switch the tray stops only processes of the same user whose executable (from
  `/proc/<pid>/exe`, not the spoofable `argv[0]`) is `rog-control-center`. It checks their start
  time, so a reused PID is never signalled. It restarts them from that executable path.
- Other processes that hold the NVIDIA card are signalled only after the user confirms a dialog
  that lists them, only when they belong to the same user, and never when they are part of the
  desktop session (by executable name). The same start-time check applies.
- Messages that include process or device names are shown as plain text, never as rich text.

## Files written

- `/var/lib/asus-gpu-tray/` (root, 0755): `pending`, `live-progress` and `live-result` are
  world-readable, so the tray can show them. `live-result` can contain the names and PIDs of
  processes that held the NVIDIA card, which any user can already see in `/proc`.
- `/etc/modprobe.d/zz-asus-gpu-tray-switch.conf`: exists only between scheduling a reboot switch
  and the next boot.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting (**Security → Report a vulnerability**) on
this repository instead of a public issue.

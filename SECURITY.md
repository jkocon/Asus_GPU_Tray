# Security

## What runs with which privileges

| Part | Runs as | Can do |
|---|---|---|
| `asus_gpu_tray.py` | the logged-in user | read sysfs and `/proc`, run `cardwire`, start the switch units, stop and restart the user's own ROG Control Center |
| `asus-gpu-switch-live` | root, `asus-gpu-live@<mode>.service` | stop and start GPU services, change device node permissions for the duration of the switch, unbind/reset/remove PCI devices, write ASUS firmware attributes |
| `asus-gpu-switch-reboot` | root, `asus-gpu-switch@<mode>.service` | write `gpu_mux_mode`, schedule a mode, blacklist the NVIDIA module for one boot, reboot |
| `asus-gpu-switch-apply` | root, at boot | apply the scheduled mode |

## The polkit rule

`/etc/polkit-1/rules.d/50-asus-gpu-tray.rules` allows starting these units without a password:

- `asus-gpu-switch@{Integrated,Hybrid,AsusEgpu,AsusMuxDgpu}.service`
- `asus-gpu-live@{Hybrid,AsusEgpu}.service`

It allows only the `start` verb, and only for a subject that is **local**, **active** and in the
`wheel` or `sudo` group, the same people who can already use `sudo`. The rule does not grant
anything beyond what they have; it only removes the password prompt.

The consequence is that any program running in an administrator's active session can reboot the
machine or trigger a ~40 s live switch without asking. This is a nuisance, not an escalation,
but remove the rule if that matters to you. The tray then asks for the password through the
normal polkit agent.

## Input handling

- The unit instance (`%i`) is the only input to the root scripts. The polkit rule restricts it to
  the names above, and each script validates it again. The mode read back from the `pending` file
  at boot is validated too.
- The root scripts do not use `eval` and do not build commands from untrusted text. The tray never
  uses a shell.
- The switch lock is `/run/asus-gpu-tray.lock` in the root-only `/run`, not the world-writable
  `/run/lock` of some distributions, so other users cannot plant or hold it.
- The tray's single-instance lock lives in `$XDG_RUNTIME_DIR`, or falls back to a private
  `~/.cache/asus-gpu-tray` (0700). It is opened with `O_NOFOLLOW` and never truncated.
- Before a live switch the tray stops only processes of the same user whose executable (from
  `/proc/<pid>/exe`, not the spoofable `argv[0]`) is `rog-control-center`. It checks their start
  time, so a reused PID is never signalled. It restarts them from that executable path.
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

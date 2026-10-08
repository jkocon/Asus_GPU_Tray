# Plan: rewrite in Rust

Status: **phase 0/1 started in [`rust/`](../rust/README.md)**; the Python tray and scripts stay in use. Goal: replace `asus_gpu_tray.py` (PyQt6) and the root shell
scripts with a single Rust binary, without losing any behaviour described in
[how-it-works.md](how-it-works.md). Python and shell are not used at runtime afterwards.

## Why

- One binary instead of Python + PyQt6: smaller package, simpler AUR `depends`, no Qt/Python
  version breakage after system updates.
- Memory and start-up: the Python/Qt tray idles at roughly 60–100 MB RSS; a `ksni` tray needs a few MB
  and starts instantly.
- Safer root code: the switch scripts run as root and drive PCI unbind/reset/remove. Types,
  explicit error handling and no shell quoting make that code easier to trust.
- Native menu: with StatusNotifierItem + DBusMenu, Plasma renders the menu like every other tray menu.

## What gets replaced

| Today | Lines | Rust replacement |
|---|---|---|
| `asus_gpu_tray.py`: tray icon, menu, `make_icon` (QPainter), `SwitchWindow`, `QMessageBox` dialogs, `QSettings`, gettext | ~1550 | `ksni` (SNI/DBusMenu tray), `tiny-skia` + `ab_glyph` (icon), `slint` (switch progress window and dialogs, built-in gettext), `serde` + `toml` (settings), `gettext-rs` (reuses `po/`), `notify-rust` |
| `scripts/asus-gpu-switch-live` | 282 | `asus-gpu-tray switch-live <mode>` |
| `scripts/asus-gpu-switch-apply` | 197 | `asus-gpu-tray switch-apply` |
| `scripts/asus-gpu-switch-reboot` | 79 | `asus-gpu-tray switch-reboot <mode>` |
| `scripts/asus-gpu-egpu-power` | 21 | `asus-gpu-tray egpu-power` |
| `tests/test_tray.py` | ~720 | `cargo test` (unit tests against a fake sysfs tree) |

External calls and their replacements:

| Call | Replacement |
|---|---|
| `systemctl start/stop/is-active`, unit checks | `zbus` → `org.freedesktop.systemd1` (StartUnit/StopUnit, wait for the job). The tray still goes through polkit (same rule). |
| `journalctl -k -f` (GPU-lost events), `journalctl` reads | `systemd` crate (`sd_journal`, links libsystemd); no subprocess |
| `/proc` scans (`own_processes`, `card_holders`, `stop_processes`) | `procfs` crate + `rustix` signals |
| sysfs reads/writes (detection, unbind, reset, remove, rescan, `egpu_enable`, `d3cold_allowed`) | plain file I/O |
| `chmod 000` on `/dev/nvidia*` | `rustix::fs::chmod` |
| `supergfxctl` | `zbus` → `org.supergfxctl.Daemon` |
| `notify-send` | `notify-rust` |
| `modprobe` | keep `modprobe` (it honours `modprobe.d`), or `kmod` crate (libkmod); decide in phase 4 |
| `udevadm settle/trigger` | keep `udevadm`, or `udev` crate (libudev monitor); decide in phase 4 |
| `nvidia-smi` (live GPU numbers) | keep `nvidia-smi`, or `nvml-wrapper`; must keep the rule "never wake a suspended dGPU" |
| `lspci` | already avoided; `pci.ids` parsed directly |
| `cardwire list --json`, `cardwire get/set` | keep the CLI (it is the cardwire API) unless `cardwired` exposes D-Bus/socket — open question |

"No Python and no shell" means no interpreter and no `sh -c`. Calling a few system binaries
(`cardwire`, possibly `modprobe`/`udevadm`/`nvidia-smi`) directly with `Command` stays allowed.

## Layout

One crate in `rust/` (next to the current code until phase 4), one binary, subcommands with `clap`:

```
src/
  main.rs            # clap: (default) tray | switch-live | switch-apply | switch-reboot | egpu-power
                     #       | dump | holders  (today: asus_gpu_tray.py --dump / --holders)
  gpu/               # shared, no UI, unit-tested (not `core`: that would shadow the core crate)
    pci.rs           # sysfs scan, class 0x03, iGPU/dGPU/eGPU rules, pci.ids names
    cardwire.rs      # list/get/set, parse_cardwire_get, cardwire_missed_dgpu
    state.rs         # GpuState, read_state, hw_modes, can_switch_live, xg_* checks, labels
    procs.rs         # card holders, own processes, stop with timeout
    journal.rs       # parse_gpu_lost, last_live_switch, kernel log follower
    systemd.rs       # zbus unit control
  tray/              # ksni menu, icon drawing, notifications, settings, i18n
  ui/                # slint: SwitchWindow, confirm/ask dialogs
  root/              # switch_live.rs, switch_apply.rs, switch_reboot.rs, egpu_power.rs
```

Units, the udev rule and the polkit rule keep their names; only `ExecStart`/`RUN` paths change.

## Phases

Each phase ends with a working, installable state. The hardware paths are only exercised on the
X16 (GV601RE) with the XG Mobile; the X13 runs without live switching (`asus-gpu-live` off).

### 0. Freeze the behaviour (Python stays)
- Record reference logs of every switch path on the X16 (`journalctl -u asus-gpu-live@*`,
  `-u asus-gpu-switch-apply`, kernel log): Hybrid → AsusEgpu, AsusEgpu → Hybrid, with apps holding
  the card, undock now, reboot switch, MUX switch, XG unplugged while docked, on battery.
- Make sure `tests/test_tray.py` covers every pure function listed in `core/`; these become the
  spec for the Rust tests.
- Capture current menu screenshots (normal, switching, XG locked, dGPU lost) for comparison.

### 1. Core library
- Port detection and state logic to `core/` with a fake sysfs root (path injected) so tests run
  anywhere.
- Port the tests 1:1. `asus-gpu-tray dump` must print the same state as `asus_gpu_tray.py --dump`
  (`dump()`) on the X16 and the X13.

### 2. Tray (scripts unchanged)
- `ksni` menu: the same sections and radio groups as `build_menu`/`radio_section`, the wake
  notification toggle, "Undock now", force refresh.
- Icon: re-create `make_icon` (GPU badge, XG marker, alert state) with `tiny-skia`; compare with
  screenshots.
- `SwitchWindow` and the `ask`/`message`/`confirm_reboot` dialogs in `slint`.
- Behaviour to keep: refresh timers and `on_menu_show` refresh, `check_wake`, `watch` +
  GPU-lost detection from the kernel log, `free_card` (close/restart RCC and other holders),
  `offer_reboot`, `on_xg_locked`/`on_xg_unlocked`, `check_live_switch` progress via the unit,
  single-instance lock, the start-up wait for the tray host (`TRAY_WAIT_S`).
- Settings: read the existing QSettings file once and migrate to
  `~/.config/asus-gpu-tray/config.toml`.
- i18n: keep `po/asus-gpu-tray.pot` and `po/pl.po`; extract strings with `xtr` (gettext-rs).
- Ship next to the Python tray: install the Rust binary, switch the autostart `.desktop`, keep
  `asus_gpu_tray.py` installed for one release as a fallback.

### 3. Root helpers, lowest risk first
For each step: the unit's `ExecStart` points to the Rust subcommand, the old script stays
installed (e.g. `/usr/local/lib/asus-gpu-tray/legacy/`) and switching back is a one-line unit
override. Run the full phase-0 matrix on the X16 before moving on.
1. `egpu-power` (21 lines; udev + after a switch): `d3cold_allowed = 0` on the XG Mobile GPU.
2. `switch-reboot`: schedule the mode, reboot.
3. `switch-apply`: boot-time apply, before the display manager and the GPU services.
4. `switch-live`, last and most carefully. Keep exactly: the order of steps, the timeouts, the
   device-node lockout (`chmod 000`) before unbind (the NVIDIA remove callback hangs while the card is
   open), root-port reset, `egpu_enable` flip, rescan and driver bind. The TERM trap becomes a signal
   handler (`signal-hook`) that restores the link, the card and the services, as the script does now
   (`TimeoutStartSec=300` stays). Progress steps must still be readable by the tray (`live_step`).

### 4. Remove Python and shell
- Drop `asus_gpu_tray.py`, `scripts/`, `tests/test_tray.py`, the PyQt6 dependency.
- `packaging/aur/PKGBUILD`: `makedepends=(cargo)`, `cargo build --release --locked`, drop
  `python-pyqt6`; regenerate `.SRCINFO`.
- `install.sh`/`uninstall.sh`, README, `how-it-works.md` (component diagram), SECURITY.md (the
  polkit rule still only allows starting the units).
- cachyos_sync: `target/apply.sh` runs `install.sh` on the X13, which now needs a build. Either
  install `rust` on the X13, or attach a release binary built in CI (GitHub Actions, pinned
  actions, checksum) and have `install.sh` download and verify it. Decide before phase 4.

## Risks

- `switch-live` encodes hardware behaviour found by trial and error (firmware call timing, KWin/RCC
  restarts, D3cold). A faithful port matters more than an idiomatic one; translate step by step,
  then refactor.
- Testing needs the real hardware; a bad switch can hang the machine until a hard reset. Have the
  legacy unit override ready, and test with unsaved work closed.
- `slint` adds a GUI toolkit for two windows; the alternative (notifications with actions only)
  would lose the progress window and the confirmations.
- Waking the dGPU: any new library (NVML, libudev enumeration) must be checked against the rule in
  how-it-works.md, "GPU detection".

## Estimate

Phase 1 and 2: the bulk of the work (~1550 lines of UI/logic plus tests). Phase 3: smaller in
lines, but most of the testing time. Plan for several sessions; phase 3 step 4 alone deserves its
own session with the XG Mobile at hand.

## Decisions (2026-10-08)

These override the text above where they differ.

| Topic | Decision |
|---|---|
| Branch | Work on the `rust` branch until phase 4. `main` (and the X13, which installs `origin/HEAD`) stays on Python. |
| supergfxd | Not ported: 2.0 drops the supergfxd fallback. Without cardwire the tray is a GPU viewer plus the ASUS hardware modes. |
| cardwire | D-Bus (`org.opengamingcollective.cardwire`: `Mode`, `Gpu/N` `Block`/`PowerState`, change signals) for reading and events; the CLI (`cardwire set`) for changing the mode. |
| Refresh | D-Bus and udev events, plus a 3 s poll of sysfs files only (asus-armoury attributes such as `egpu_connected` have no events). No `cardwire` process per poll. |
| Windows | `slint` for the switch progress window and the dialogs. |
| Kernel log | Keep the `journalctl -k -f` child process (no shell involved). |
| System binaries | Keep `modprobe`, `udevadm`, `nvidia-smi` (called directly, no shell). libudev only to listen for device events. |
| Translations | `gettext-rs`; `po/` → `msgfmt` → `locale/` unchanged. |
| Settings | Read and write the existing QSettings INI file (`~/.config/asus-gpu-tray/asus-gpu-tray.conf`, `notify_dgpu_wake`); no migration. |
| Testing the tray (phase 2) | Same single-instance lock as the Python tray; switch by overriding the autostart entry in `~/.config/autostart/`. |
| `dump` parity | Compared with `asus_gpu_tray.py --dump` minus the `supergfxd` line. |
| Phase 0 recordings | Right before phase 3, on the X16 with the XG Mobile. |
| CI | GitHub Actions on `rust/`: fmt, clippy `-D warnings`, test; actions pinned by SHA; publishes nothing. |
| X13 delivery | For now: release binary built on the X16, uploaded as a GitHub release asset; `install.sh` downloads it and checks its SHA256. Later: the AUR package (AUR account registration is closed at the moment). |

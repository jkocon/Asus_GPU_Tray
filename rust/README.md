# Asus GPU Tray – Rust rewrite (work in progress)

The Rust version is built here, next to the working Python tray and shell scripts, which stay in
use until the Rust binary replaces them step by step. Plan and phases:
[../docs/rust-rewrite-plan.md](../docs/rust-rewrite-plan.md).

## Status

| Phase | State |
|---|---|
| 0. Freeze the behaviour | `tests/reference/x16-hybrid-dump.txt`; switch recordings with `tools/record-reference` still to do (needs the XG Mobile) |
| 1. Core library (`src/gpu/`) | done: detection, state, labels, cardwire over D-Bus, processes, kernel-log parsing, `dump`, `holders` |
| 2. Tray (`src/tray/`) | first cut: icon, menu, dialogs, switch flows; to be tested on the X16 with the XG Mobile |
| 3. Root helpers | not started |
| 4. Remove Python and shell | not started |

## Build and test

```sh
cd rust
cargo test
cargo build --release
./target/release/asus-gpu-tray dump
```

Compare with the Python tray on real hardware (must print nothing):

```sh
diff <(python3 ../asus_gpu_tray.py --dump | grep -v "^supergfxd") <(./target/release/asus-gpu-tray dump)
diff <(python3 ../asus_gpu_tray.py --holders) <(./target/release/asus-gpu-tray holders)
```

## Layout

```
src/main.rs        no subcommand: the tray; dump, holders (switch-* and egpu-power come later)
src/gpu/           shared core, no UI; mirrors the pure functions of asus_gpu_tray.py
  paths.rs         system paths (tests point them at a fake tree)
  cmd.rs           external commands behind a trait, so tests see every call
  pci.rs           sysfs scan, iGPU/dGPU/eGPU rules, pci.ids names
  cardwire.rs      cardwire list/get parsing, missed-dGPU check
  state.rs         GpuState, read_state with the cardwired repair, live-switch rules
  labels.rs        menu texts and dump
  procs.rs         card holders, stopping and restarting processes
  journal.rs       GPU-lost kernel messages
  stats.rs         nvidia-smi numbers (never for a sleeping card)
  i18n.rs          gettext (tr/trf), msgids of the Python tray
  cardwire_dbus.rs cardwired over D-Bus
src/tray/          the tray (ksni icon and menu, slint windows, software renderer)
  app.rs           GpuTray: refresh, watch, switch flows (async on the slint event loop)
  menu.rs          menu as data (tested) + the ksni model
  dialogs.rs       message boxes, live-switch progress window
  icon.rs          tray icon (resvg), as make_icon
  events.rs        kernel log, cardwired signals, udev PCI events
  settings.rs, lock.rs, notify.rs
```

## Phase 0 recordings

`tools/record-reference --list` shows the scenarios. For each one: run
`tools/record-reference <scenario>`, do it in the tray, press Enter. Scenarios that end in a
reboot are finished with `--after-boot <scenario>`. Results go to `tests/reference/x16/`. The
script only reads (sysfs, journal, D-Bus) and never wakes the dGPU.

## Trying the tray on the X16

Both trays share the single-instance lock, so only one runs. To use the Rust one at login,
override the autostart entry; to go back, delete the file:

```sh
mkdir -p ~/.config/autostart
sed "s|^Exec=.*|Exec=$PWD/target/release/asus-gpu-tray|" /etc/xdg/autostart/asus-gpu-tray.desktop \
    > ~/.config/autostart/asus-gpu-tray.desktop
```

The module is called `gpu`, not `core` as in the first draft of the plan, so it does not shadow
Rust's `core` crate.

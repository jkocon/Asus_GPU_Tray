# Asus GPU Tray – Rust rewrite (work in progress)

The Rust version is built here, next to the working Python tray and shell scripts, which stay in
use until the Rust binary replaces them step by step. Plan and phases:
[../docs/rust-rewrite-plan.md](../docs/rust-rewrite-plan.md).

## Status

| Phase | State |
|---|---|
| 0. Freeze the behaviour | started: `tests/reference/x16-hybrid-dump.txt` (X16, Hybrid, XG Mobile not connected) |
| 1. Core library (`src/gpu/`) | in progress: detection, state, labels, cardwire, processes, kernel-log parsing, `dump`, `holders` |
| 2. Tray | not started (`asus-gpu-tray` without a subcommand only prints a note) |
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
src/main.rs        subcommands: dump, holders (tray, switch-* and egpu-power come later)
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
  i18n.rs          tr()/fill(); English until gettext is wired up in phase 2
```

The module is called `gpu`, not `core` as in the first draft of the plan, so it does not shadow
Rust's `core` crate.

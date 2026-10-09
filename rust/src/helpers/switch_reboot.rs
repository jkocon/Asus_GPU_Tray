//! `switch-reboot <mode>`, port of scripts/asus-gpu-switch-reboot. Run as root by
//! asus-gpu-switch@<mode>.service. Schedules a mode change for the next boot and reboots
//! immediately. The change itself is done by switch-apply early during boot. Used for the MUX
//! mode and as the fallback when a live switch cannot run.

use std::fs;
use std::time::Duration;

use super::{try_lock, Ops};
use crate::gpu::paths::{read, sorted_entries, Paths};

pub const MODES: [&str; 4] = ["Integrated", "Hybrid", "AsusEgpu", "AsusMuxDgpu"];

/// After the NVIDIA driver has lost a GPU (an XG Mobile unlocked while in use, Xid 79), it wedges:
/// nvidia-modeset waits forever for the dead GPU, and every process that closes the device hangs
/// in the kernel. A normal shutdown then never finishes (seen twice on a GV601RE), so reboot the
/// emergency way instead: flush and remount the disks read-only, then reset right away.
pub fn gpu_lost(paths: &Paths, ops: &dyn Ops) -> bool {
    let nvidia_on_bus = sorted_entries(&paths.pci).iter().any(|d| read(&d.join("vendor")) == "0x10de");
    if !nvidia_on_bus && paths.read_attr("egpu_enable") == "1" {
        return true; // XG Mobile mode, but its GPU is gone from the bus
    }
    ops.kernel_lost_gpu()
}

/// Exit code as the script's: 2 unknown mode, 3 XG Mobile not locked, 1 other refusals.
pub fn run(target: &str, paths: &Paths, ops: &dyn Ops) -> u8 {
    if !MODES.contains(&target) {
        ops.say(&format!("Unknown mode: '{target}'"));
        return 2;
    }
    if target == "AsusEgpu" && paths.read_attr("egpu_connected") != "1" {
        ops.say("XG Mobile is not connected/locked - aborting");
        return 3;
    }

    // Kept open until the process ends, like the script's `exec 9>`.
    let _lock = match try_lock(&paths.switch_lock) {
        Ok(Some(file)) => Some(file),
        Ok(None) => {
            // A live switch that ran into a lost GPU hangs in the kernel for good and keeps the lock.
            if !gpu_lost(paths, ops) {
                ops.say("Another GPU switch is already running - aborting");
                return 1;
            }
            ops.say("Another GPU switch holds the lock, but a GPU was lost (that switch is hung) - going on");
            None
        }
        Err(e) => {
            ops.say(&format!("Cannot open {}: {e}", paths.switch_lock.display()));
            return 1;
        }
    };

    // MUX is a firmware setting applied only after a reboot, so writing it now is safe. It goes
    // first: if it fails, nothing is scheduled.
    let mux_file = paths.attr.join("gpu_mux_mode/current_value");
    if mux_file.exists() {
        let mux = read(&mux_file);
        if target == "AsusMuxDgpu" && mux != "0" {
            // The firmware answers EBUSY while the XG Mobile is active.
            if fs::write(&mux_file, "0").is_err() {
                ops.say("The firmware refused the MUX mode - switch to the built-in dGPU first");
                return 1;
            }
        } else if target != "AsusMuxDgpu" && mux == "0" {
            if let Err(e) = fs::write(&mux_file, "1") {
                ops.say(&format!("Writing gpu_mux_mode failed: {e}"));
                return 1;
            }
        }
    }

    let pending = fs::create_dir_all(paths.state_dir()).and_then(|_| fs::write(&paths.pending, format!("{target}\n")));
    if let Err(e) = pending {
        ops.say(&format!("Cannot write {}: {e}", paths.pending.display()));
        return 1;
    }

    // No NVIDIA blacklist any more: switch-apply lets the driver come up on the current card and
    // then switches the way the live switch does.
    ops.sync();

    if gpu_lost(paths, ops) {
        ops.say(&format!(
            "Scheduled mode {target}; the NVIDIA driver lost a GPU, so a normal shutdown would hang - emergency reboot"
        ));
        ops.sleep(Duration::from_secs(1)); // let the message reach the journal
        ops.sync();
        // Writes to the trigger work whatever kernel.sysrq says.
        for (key, pause) in [('s', 2), ('u', 2), ('b', 0)] {
            ops.sysrq(paths, key); // sync, remount everything read-only, reset now
            ops.sleep(Duration::from_secs(pause));
        }
    }

    ops.say(&format!("Scheduled mode {target}, rebooting"));
    ops.reboot()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::testutil::{attr, nvidia_function, FakeOps};

    fn setup() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        fs::create_dir_all(paths.switch_lock.parent().unwrap()).unwrap();
        attr(&paths, "egpu_enable", "0");
        attr(&paths, "egpu_connected", "1");
        attr(&paths, "gpu_mux_mode", "1");
        nvidia_function(&paths, "0000:01:00.0");
        (tmp, paths)
    }

    #[test]
    fn schedules_and_reboots() {
        let (_tmp, paths) = setup();
        let ops = FakeOps::default();
        assert_eq!(run("AsusEgpu", &paths, &ops), 0);
        assert_eq!(read(&paths.pending), "AsusEgpu");
        assert_eq!(ops.log(), ["sync", "say Scheduled mode AsusEgpu, rebooting", "reboot"]);
    }

    #[test]
    fn refusals() {
        let (_tmp, paths) = setup();
        let ops = FakeOps::default();
        assert_eq!(run("Bogus", &paths, &ops), 2);
        attr(&paths, "egpu_connected", "0");
        assert_eq!(run("AsusEgpu", &paths, &ops), 3);
        assert!(!paths.pending.exists());
        assert!(!ops.log().contains(&"reboot".to_string()));
    }

    #[test]
    fn mux_written_both_ways() {
        let (_tmp, paths) = setup();
        let ops = FakeOps::default();
        run("AsusMuxDgpu", &paths, &ops);
        assert_eq!(paths.read_attr("gpu_mux_mode"), "0");
        run("Hybrid", &paths, &ops);
        assert_eq!(paths.read_attr("gpu_mux_mode"), "1");
    }

    #[test]
    fn lock_held_by_a_running_switch() {
        let (_tmp, paths) = setup();
        let _held = try_lock(&paths.switch_lock).unwrap().unwrap();
        let ops = FakeOps::default();
        assert_eq!(run("Hybrid", &paths, &ops), 1);
        assert!(!paths.pending.exists());
        // ...but a switch hung on a lost GPU must not block the emergency reboot.
        let ops = FakeOps { kernel_lost: true, ..Default::default() };
        assert_eq!(run("Hybrid", &paths, &ops), 0);
        assert!(ops.log().contains(&"sysrq b".to_string()));
    }

    #[test]
    fn emergency_reboot_after_a_gpu_loss() {
        let (_tmp, paths) = setup();
        // XG Mobile mode with no NVIDIA function on the bus counts as lost, no kernel message needed.
        attr(&paths, "egpu_enable", "1");
        fs::remove_dir_all(paths.pci.join("0000:01:00.0")).unwrap();
        let ops = FakeOps::default();
        assert_eq!(run("Hybrid", &paths, &ops), 0);
        let log = ops.log();
        let sysrq: Vec<&String> = log.iter().filter(|l| l.starts_with("sysrq")).collect();
        assert_eq!(sysrq, ["sysrq s", "sysrq u", "sysrq b"]);
        assert_eq!(read(&paths.pending), "Hybrid");
    }
}

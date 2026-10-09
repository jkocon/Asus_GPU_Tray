//! `switch-apply`, port of scripts/asus-gpu-switch-apply. Run at boot by
//! asus-gpu-switch-apply.service, before the login screen and the GPU services. Applies the mode
//! scheduled by switch-reboot: loads the NVIDIA driver on the current card and re-routes the
//! eGPU/dGPU with switch-live (the same steps as a live switch), and sets the mode in supergfxd's
//! config when supergfxd is installed.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Duration;

use super::pcie::{self, driver, nvidia_bound, nvidia_devices, parent_port, valid_bdf, Port};
use super::switch_reboot::MODES;
use super::{set_supergfxd_mode, Ops};
use crate::gpu::paths::{file_name, read, Paths};

pub fn run(paths: &Paths, ops: &dyn Ops) -> u8 {
    let code = apply(paths, ops);
    cleanup(paths, ops);
    code
}

/// Whatever the outcome (the script's EXIT trap): a one-boot NVIDIA blacklist left by older
/// versions goes away. supergfxd loads the driver itself; without it, replay udev "add" events so
/// the driver loads for the (new) NVIDIA card.
fn cleanup(paths: &Paths, ops: &dyn Ops) {
    let _ = fs::remove_file(&paths.switch_blacklist);
    if ops.run(&["systemctl", "-q", "is-enabled", "supergfxd.service"]).is_none() {
        // udevd keeps the modprobe config it read at startup (with the blacklist) for a while, and
        // when this unit finishes quickly the replayed events still find NVIDIA blacklisted. Then
        // nvidia-powerd loaded nvidia without its softdeps, and nvidia_drm was missing for the
        // whole boot. So: reload udev first, and load nvidia_drm explicitly - in the background, so
        // the login screen never waits for the NVIDIA driver.
        ops.run(&["udevadm", "control", "--reload"]);
        ops.run(&["udevadm", "trigger", "--action=add", "--subsystem-match=pci", "--attr-match=vendor=0x10de"]);
        ops.run(&["systemctl", "--no-block", "start", "modprobe@nvidia_drm.service"]);
    }
}

/// A line on the text console; errors ignored, as the script's `2>/dev/null`.
fn console(paths: &Paths, text: &str) {
    if let Ok(mut tty) = OpenOptions::new().append(true).open(&paths.console) {
        let _ = tty.write_all(text.as_bytes());
    }
}

fn apply(paths: &Paths, ops: &dyn Ops) -> u8 {
    if !paths.pending.exists() {
        return 0;
    }
    let mut target = read(&paths.pending);
    let _ = fs::remove_file(&paths.pending);
    if !MODES.contains(&target.as_str()) {
        ops.say(&format!("Ignoring unknown scheduled mode '{target}'"));
        return 1;
    }

    let egpu_enable = paths.attr.join("egpu_enable/current_value");
    for _ in 0..100 {
        if egpu_enable.exists() {
            break;
        }
        ops.sleep(Duration::from_millis(200));
    }
    if !egpu_enable.exists() {
        ops.say("No asus-armoury attributes - skipping mode change");
        return 1;
    }

    let mut want_egpu = "0";
    if target == "AsusEgpu" {
        if paths.read_attr("egpu_connected") == "1" {
            want_egpu = "1";
        } else {
            ops.say("XG Mobile disconnected - setting Hybrid instead of AsusEgpu");
            target = "Hybrid".into();
        }
    }

    // Preferred: switch the way the live switch does. Loading the NVIDIA driver fresh right after
    // the firmware switch deadlocked inside the driver (2 of 3 boots on a GV601RE): GSP init and the
    // ACPI NVPCF notifications the firmware sends after the switch wait for the same RM lock, and
    // every later module load (sound, Bluetooth) waits behind it. A driver that was already running
    // never did, in many live switches. So let the driver come up on the card that is there now,
    // like on any normal boot, and run the live switch before the login screen starts. If it fails,
    // stay in the current mode rather than risk the direct path below.
    if paths.read_attr("egpu_enable") != want_egpu && !nvidia_devices(paths).is_empty() {
        if let Some(code) = live_switch(paths, ops, want_egpu) {
            return code;
        }
    }

    // Fallback when no NVIDIA card is visible at all (XG Mobile mode with the dock unplugged; the
    // firmware normally switches back by itself before Linux starts then). The live switch never
    // failed to bring up the new card; this path, without its reset and link cycle, once left the
    // NVIDIA driver hung while probing the built-in dGPU (after a warm reset), and the login screen
    // hung with it. So the same steps are done here: reset the card from its root port and take the
    // link down around the firmware switch, then wait for the link before rescanning.
    if paths.read_attr("egpu_enable") != want_egpu {
        if let Some(code) = direct_switch(paths, ops, want_egpu) {
            return code;
        }
    }

    match set_supergfxd_mode(paths, &target) {
        Ok(false) => {
            ops.say(&format!("Hardware mode set to {target} (no supergfxd config)"));
            0
        }
        Ok(true) => {
            ops.say(&format!("Mode set to {target}"));
            0
        }
        Err(e) => {
            ops.say(&format!("Updating {} failed: {e}", paths.supergfxd_conf.display()));
            1
        }
    }
}

/// Some(exit code) when the boot has to stop here.
fn live_switch(paths: &Paths, ops: &dyn Ops, want_egpu: &str) -> Option<u8> {
    let live_target = if want_egpu == "1" { "AsusEgpu" } else { "Hybrid" };
    // The login screen waits for this (~40 s, most of it in the firmware). The splash went black
    // while the GPUs changed, so stop it and write to the text console instead: the first output
    // makes the console (fbcon on the iGPU) take over the screen, whatever the splash did.
    ops.run(&["plymouth", "quit"]);
    let gpu = if want_egpu == "1" { "XG Mobile" } else { "built-in dGPU" };
    console(
        paths,
        &format!(
            "\x1b[2J\x1b[H\n  Asus GPU Tray: switching graphics to the {gpu}\n  This takes about 40 seconds. \
             Do not turn off the computer or disconnect the XG Mobile.\n\n"
        ),
    );
    let _ = fs::remove_file(&paths.switch_blacklist); // left by older versions of switch-reboot
    ops.run(&["udevadm", "control", "--reload"]);
    ops.say("Loading the NVIDIA driver on the current card");
    ops.run(&["modprobe", "nvidia"]); // with its softdeps (nvidia-uvm, nvidia-drm)
    let mut bound = false;
    for _ in 0..120 {
        // up to 60 s
        if nvidia_bound(paths).is_some() {
            bound = true;
            break;
        }
        ops.sleep(Duration::from_millis(500));
    }
    if !bound {
        ops.say("The NVIDIA driver did not come up within 60 s - staying in the current mode");
        return Some(1);
    }
    // Each step of the live switch goes to the journal and to the console.
    let live = paths.lib_dir.join("asus-gpu-switch-live");
    let rc = ops.stream(&[&live.to_string_lossy(), live_target], &mut |line| {
        ops.say(line);
        console(paths, &format!("  {line}\n"));
    });
    if rc != 0 {
        console(paths, "  The switch failed - starting in the current mode.\n");
        ops.say(&format!(
            "The live switch at boot failed (exit {rc}, see live-progress) - staying in the current mode"
        ));
        return Some(1);
    }
    None
}

/// Some(exit code) when the boot has to stop here.
fn direct_switch(paths: &Paths, ops: &dyn Ops, want_egpu: &str) -> Option<u8> {
    let devices = nvidia_devices(paths);
    for d in &devices {
        // snd_hda_intel on the HDMI audio function can be unbound safely - only nvidia is dangerous
        if driver(d) == "nvidia" {
            ops.say(&format!("{} already has the driver loaded - aborting to avoid a system hang", file_name(d)));
            return Some(1);
        }
    }

    // The NVIDIA card's root port; remembered for boots where no card is visible (dock unplugged).
    let bdf = match devices.first() {
        Some(d) => parent_port(d),
        None => read(&paths.root_port_file()),
    };
    let port = Port::new(&bdf, ops);
    let port = if valid_bdf(&bdf) && port.controllable() {
        let _ = fs::write(paths.root_port_file(), format!("{bdf}\n"));
        Some(port)
    } else {
        let shown = if bdf.is_empty() { "unknown" } else { &bdf };
        ops.say(&format!("No controllable root port ({shown}) - switching without the reset and link cycle"));
        None
    };

    for d in &devices {
        if d.join("driver").exists() {
            let _ = fs::write(d.join("driver/unbind"), file_name(d));
        }
    }
    if let Some(port) = &port {
        let reset = paths.pci.join(&port.bdf).join("reset_subordinate");
        if reset.exists() {
            ops.say(&format!("secondary bus reset below {}", port.bdf));
            if fs::write(&reset, "1").is_err() {
                ops.say(&format!("the bus reset below {} failed", port.bdf));
            }
            ops.sleep(Duration::from_secs(1));
        }
    }
    for d in &devices {
        let _ = fs::write(d.join("remove"), "1");
    }
    if let Some(port) = &port {
        ops.say(&format!("link down on {}", port.bdf));
        port.disable_link();
        ops.sleep(Duration::from_millis(500));
    }

    ops.say(&format!("egpu_enable -> {want_egpu}"));
    if let Err(e) = fs::write(paths.attr.join("egpu_enable/current_value"), want_egpu) {
        ops.say(&format!("writing egpu_enable failed: {e}"));
    }
    let dgpu_disable = paths.attr.join("dgpu_disable/current_value");
    if want_egpu == "0" && dgpu_disable.exists() {
        let _ = fs::write(&dgpu_disable, "0");
    }
    ops.sleep(Duration::from_secs(2));

    if let Some(port) = &port {
        port.enable_link();
        if port.wait_link() {
            ops.say(&format!("link up on {}", port.bdf));
        } else {
            ops.say(&format!("the link on {} did not come up", port.bdf));
        }
        port.clear_aer();
    }
    let _ = pcie::rescan(paths);
    ops.sleep(Duration::from_secs(1));
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::testutil::{attr, nvidia_function, FakeOps};
    use std::os::unix::fs::symlink;

    fn setup(pending: &str) -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        fs::create_dir_all(paths.state_dir()).unwrap();
        fs::write(&paths.pending, format!("{pending}\n")).unwrap();
        fs::write(&paths.console, "").unwrap();
        attr(&paths, "egpu_enable", "0");
        attr(&paths, "egpu_connected", "1");
        attr(&paths, "dgpu_disable", "0");
        (tmp, paths)
    }

    fn bind_nvidia(paths: &Paths, addr: &str) {
        let drv = paths.pci.parent().unwrap().join("drivers/nvidia");
        fs::create_dir_all(&drv).unwrap();
        symlink(&drv, paths.pci.join(addr).join("driver")).unwrap();
    }

    #[test]
    fn nothing_pending_only_cleans_up() {
        let (_tmp, paths) = setup("x");
        fs::remove_file(&paths.pending).unwrap();
        fs::create_dir_all(paths.switch_blacklist.parent().unwrap()).unwrap();
        fs::write(&paths.switch_blacklist, "blacklist nvidia\n").unwrap();
        let ops = FakeOps::default();
        assert_eq!(run(&paths, &ops), 0);
        assert!(!paths.switch_blacklist.exists());
        assert_eq!(ops.commands()[0], "systemctl -q is-enabled supergfxd.service");
        assert!(ops.commands().contains(&"systemctl --no-block start modprobe@nvidia_drm.service".to_string()));

        // supergfxd loads the driver itself.
        let ops = FakeOps::default();
        ops.answer("systemctl -q is-enabled supergfxd", Some(""));
        run(&paths, &ops);
        assert_eq!(ops.commands(), ["systemctl -q is-enabled supergfxd.service"]);
    }

    #[test]
    fn unknown_mode_is_dropped() {
        let (_tmp, paths) = setup("Bogus");
        let ops = FakeOps::default();
        assert_eq!(run(&paths, &ops), 1);
        assert!(!paths.pending.exists());
        assert!(ops.log().contains(&"say Ignoring unknown scheduled mode 'Bogus'".to_string()));
    }

    #[test]
    fn same_mode_changes_no_hardware() {
        let (_tmp, paths) = setup("Hybrid");
        nvidia_function(&paths, "0000:01:00.0");
        let ops = FakeOps::default();
        assert_eq!(run(&paths, &ops), 0);
        assert!(ops.log().contains(&"say Hardware mode set to Hybrid (no supergfxd config)".to_string()));
        assert!(!ops.commands().iter().any(|c| c.starts_with("modprobe") || c.starts_with("setpci")));
    }

    #[test]
    fn xg_mobile_gone_falls_back_to_hybrid() {
        let (_tmp, paths) = setup("AsusEgpu");
        attr(&paths, "egpu_connected", "0");
        fs::write(&paths.supergfxd_conf, r#"{"mode": "Integrated", "vfio_enable": false, "pending_mode": "x"}"#)
            .unwrap();
        let ops = FakeOps::default();
        assert_eq!(run(&paths, &ops), 0);
        let cfg: serde_json::Value = serde_json::from_str(&fs::read_to_string(&paths.supergfxd_conf).unwrap()).unwrap();
        assert_eq!(
            cfg,
            serde_json::json!({"mode": "Hybrid", "vfio_enable": false, "pending_mode": null, "pending_action": null})
        );
    }

    #[test]
    fn switches_through_the_live_helper() {
        let (_tmp, paths) = setup("AsusEgpu");
        nvidia_function(&paths, "0000:01:00.0");
        bind_nvidia(&paths, "0000:01:00.0");
        let flip = paths.clone();
        let ops = FakeOps {
            streamed: (vec!["unbind 0000:01:00.0".into(), "Switched".into()], 0),
            on_stream: Some(Box::new(move || attr(&flip, "egpu_enable", "1"))),
            ..Default::default()
        };
        assert_eq!(run(&paths, &ops), 0);
        let live = paths.lib_dir.join("asus-gpu-switch-live");
        assert!(ops.log().contains(&format!("stream {} AsusEgpu", live.display())));
        assert!(ops.commands().contains(&"modprobe nvidia".to_string()));
        let tty = fs::read_to_string(&paths.console).unwrap();
        assert!(tty.contains("switching graphics to the XG Mobile"));
        assert!(tty.contains("  unbind 0000:01:00.0\n"));
    }

    #[test]
    fn failed_live_switch_stays_in_the_current_mode() {
        let (_tmp, paths) = setup("AsusEgpu");
        nvidia_function(&paths, "0000:01:00.0");
        bind_nvidia(&paths, "0000:01:00.0");
        let ops = FakeOps { streamed: (vec![], 4), ..Default::default() };
        assert_eq!(run(&paths, &ops), 1);
        assert!(!ops.commands().iter().any(|c| c.starts_with("setpci")), "no direct switch after a failed live one");
        assert!(fs::read_to_string(&paths.console).unwrap().contains("The switch failed"));
    }

    #[test]
    fn driver_that_never_binds_aborts() {
        let (_tmp, paths) = setup("AsusEgpu");
        nvidia_function(&paths, "0000:01:00.0");
        let ops = FakeOps::default();
        assert_eq!(run(&paths, &ops), 1);
        assert!(!ops.log().iter().any(|l| l.starts_with("stream")));
    }

    #[test]
    fn no_card_switches_directly_with_the_remembered_port() {
        let (_tmp, paths) = setup("Hybrid");
        attr(&paths, "egpu_enable", "1");
        fs::write(paths.root_port_file(), "0000:00:01.1\n").unwrap();
        fs::create_dir_all(paths.pci.join("0000:00:01.1")).unwrap();
        fs::write(paths.pci.join("0000:00:01.1/reset_subordinate"), "").unwrap();
        let ops = FakeOps::default();
        ops.answer("setpci -s 0000:00:01.1 CAP_EXP+0x10.w=", Some(""));
        ops.answer("setpci -s 0000:00:01.1 CAP_EXP+0x10.w", Some("0040"));
        ops.answer("setpci -s 0000:00:01.1 CAP_EXP+0x12.w", Some("2000"));
        ops.answer("setpci -s 0000:00:01.1 ECAP_AER", Some(""));
        assert_eq!(run(&paths, &ops), 0);
        assert_eq!(paths.read_attr("egpu_enable"), "0");
        let setpci: Vec<String> = ops.commands().into_iter().filter(|c| c.starts_with("setpci")).collect();
        assert_eq!(
            setpci,
            [
                "setpci -s 0000:00:01.1 CAP_EXP+0x10.w",
                "setpci -s 0000:00:01.1 CAP_EXP+0x10.w=0010:0010",
                "setpci -s 0000:00:01.1 CAP_EXP+0x10.w=0000:0010",
                "setpci -s 0000:00:01.1 CAP_EXP+0x12.w",
                "setpci -s 0000:00:01.1 ECAP_AER+0x04.l=ffffffff",
            ]
        );
        let log = ops.log();
        let pos = |s: &str| log.iter().position(|l| l == s).unwrap_or_else(|| panic!("{s} missing"));
        assert!(pos("say secondary bus reset below 0000:00:01.1") < pos("say link down on 0000:00:01.1"));
        assert!(pos("say link down on 0000:00:01.1") < pos("say egpu_enable -> 0"));
        assert!(pos("say egpu_enable -> 0") < pos("say link up on 0000:00:01.1"));
        assert_eq!(read(&paths.pci.parent().unwrap().join("rescan")), "1");
    }

    #[test]
    fn direct_switch_refuses_a_card_with_the_driver() {
        let (_tmp, paths) = setup("Hybrid");
        attr(&paths, "egpu_enable", "1");
        nvidia_function(&paths, "0000:01:00.0");
        bind_nvidia(&paths, "0000:01:00.0");
        // The live switch "succeeded" without flipping egpu_enable; the direct path must then
        // refuse because the driver is bound.
        let ops = FakeOps { streamed: (vec![], 0), ..Default::default() };
        assert_eq!(run(&paths, &ops), 1);
        assert!(ops
            .log()
            .contains(&"say 0000:01:00.0 already has the driver loaded - aborting to avoid a system hang".to_string()));
        assert_eq!(paths.read_attr("egpu_enable"), "1");
    }
}

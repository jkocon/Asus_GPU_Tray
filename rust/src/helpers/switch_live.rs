//! `switch-live <mode>`, port of scripts/asus-gpu-switch-live. Run as root by
//! asus-gpu-live@<mode>.service.
//!
//! Switches between the built-in dGPU (Hybrid) and the XG Mobile (AsusEgpu) without a reboot: stop
//! services that hold the NVIDIA card, make sure nothing else holds it, unbind the NVIDIA PCI
//! functions, reset them from the root port, remove them, flip egpu_enable, rescan PCI and let the
//! (still loaded) driver bind to the new card. About 40 s on a ROG Flow X16 GV601RE, most of it
//! inside the firmware call.
//!
//! What makes it work:
//! - The NVIDIA driver waits forever in its PCI remove callback while any process has the card
//!   open, which hangs the system. So the device nodes are made inaccessible (chmod 000) and the
//!   switch is refused while anything still holds /dev/nvidia* or the card's DRM nodes - the
//!   compositor included (see README: KWin setup).
//! - Flipping egpu_enable while the card is still in the state the driver left it in resets the
//!   whole machine ("an uncorrected error caused a data fabric sync flood event"), also after an
//!   FLR. It worked every time after a secondary bus reset from the root port followed by a link
//!   down/up cycle (Link Disable, which pciehp undoes within a second: "Link Down", "Card
//!   present"). Which of the two is essential was not isolated, so both are done and the switch is
//!   aborted before the firmware call if the bus reset fails. At boot the card has not been touched
//!   by the driver yet, which is why the boot-time fallback in switch-apply needs neither.
//! - Masking Surprise Down (below) was part of that sequence too, but the GV601RE's root port has
//!   no Surprise Down reporting (LnkCap "Surprise-"), so the write has no effect there. It is kept
//!   for ports that do report it.
//!
//! Progress goes to /var/lib/asus-gpu-tray/live-progress with a sync after every step, so after a
//! hard hang the last step shows where it stopped. The step texts are what the tray's progress
//! window recognizes (switch_stage), so they stay as in the script.

use std::collections::{BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::pcie::{self, driver, nvidia_bound, nvidia_devices, valid_bdf, Port};
use super::{egpu_power, set_supergfxd_mode, try_lock, Ops};
use crate::gpu::paths::{file_name, read, sorted_entries, Paths};
use crate::gpu::procs::nvidia_nodes;

const SERVICES: [&str; 4] =
    ["cardwired.service", "nvidia-powerd.service", "nvidia-persistenced.service", "supergfxd.service"];
const INTERRUPTED: &str = "Interrupted - the hardware was put back as far as possible; reboot if the GPU is missing";

/// Exit code and the message for the tray (live-result).
type Finish = (u8, String);

fn fin(code: u8, msg: impl Into<String>) -> Finish {
    (code, msg.into())
}

pub fn run(target: &str, paths: &Paths, ops: &dyn Ops) -> u8 {
    let _ = fs::create_dir_all(paths.state_dir());
    let _ = fs::write(&paths.live_progress, "");
    let mut live = Live {
        paths,
        ops,
        _lock: None,
        stopped: Vec::new(),
        node_modes: Vec::new(),
        devices: Vec::new(),
        root: String::new(),
        uemsk: None,
        link_down: false,
        detached: false,
    };
    let (code, msg) = live.switch(target).unwrap_or_else(|f| f);
    live.finish(code, &msg)
}

struct Live<'a> {
    paths: &'a Paths,
    ops: &'a dyn Ops,
    /// The switch lock, held until the process ends.
    _lock: Option<File>,
    stopped: Vec<&'static str>,
    /// Device node, its permission bits and device number before chmod 000.
    node_modes: Vec<(PathBuf, u32, u64)>,
    devices: Vec<PathBuf>,
    root: String,
    /// The port's AER uncorrectable error mask before the switch; None without AER.
    uemsk: Option<String>,
    link_down: bool,
    detached: bool,
}

/// The device nodes chmod 000 applies to: character devices (the test tree has plain files).
fn is_node(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.file_type().is_char_device() || (cfg!(test) && m.is_file()))
}

/// "comm (pid)" of every process but this one with one of the nodes open. Every process is looked
/// at, whatever its executable, as the script's find over /proc/<pid>/fd: one missed holder hangs
/// the machine in the driver's remove callback.
fn holders(proc: &Path, nodes: &HashSet<String>) -> Vec<String> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let me = std::process::id().to_string();
    let mut out = Vec::new();
    for dir in sorted_entries(proc) {
        let pid = file_name(&dir);
        if !pid.bytes().all(|c| c.is_ascii_digit()) || pid == me {
            continue;
        }
        let Ok(fds) = fs::read_dir(dir.join("fd")) else { continue };
        let open =
            fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|t| nodes.contains(&*t.to_string_lossy())));
        if open {
            out.push(format!("{} ({pid})", read(&dir.join("comm"))));
        }
    }
    out
}

/// The local time as HH:MM:SS (`date +%T`).
fn clock() -> String {
    // SAFETY: time with a null pointer and localtime_r into a zeroed struct we own.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

impl Live<'_> {
    /// A step for the journal and live-progress, synced to disk.
    fn record(&self, msg: &str) {
        self.ops.say(msg);
        if let Ok(mut f) = OpenOptions::new().append(true).create(true).open(&self.paths.live_progress) {
            let _ = writeln!(f, "{} {msg}", clock());
        }
        self.ops.sync();
    }

    /// record, then stop the switch when a signal arrived (where the script's trap would run).
    fn step(&self, msg: &str) -> Result<(), Finish> {
        self.record(msg);
        self.check()
    }

    fn check(&self) -> Result<(), Finish> {
        if self.ops.interrupted() {
            return Err(fin(7, INTERRUPTED));
        }
        Ok(())
    }

    fn pause(&self, d: Duration) -> Result<(), Finish> {
        self.ops.sleep(d);
        self.check()
    }

    fn port(&self) -> Port<'_> {
        Port::new(&self.root, self.ops)
    }

    fn link_up(&mut self) {
        self.record(&format!("enable link on {}", self.root));
        let port = self.port();
        port.enable_link();
        if !port.wait_link() {
            self.record(&format!("link on {} did not come up", self.root));
        }
        if let Some(uemsk) = &self.uemsk {
            port.clear_aer();
            port.set_aer_mask(uemsk);
        }
        self.link_down = false;
    }

    /// Brings back functions that were removed or left without a driver.
    fn reattach(&mut self) {
        self.record("Rescanning PCI");
        let _ = pcie::rescan(self.paths);
        for d in &self.devices {
            if d.exists() && !d.join("driver").exists() {
                pcie::probe(self.paths, d);
            }
        }
        self.detached = false;
    }

    fn restore_nodes(&mut self) {
        for (node, mode, rdev) in self.node_modes.drain(..) {
            // DRM nodes are recreated for the new card; only restore nodes that are still the same device.
            if is_node(&node) && fs::metadata(&node).is_ok_and(|m| m.rdev() == rdev) {
                let _ = fs::set_permissions(&node, fs::Permissions::from_mode(mode));
            }
        }
    }

    /// Puts back what the switch changed, as far as it got, and reports the outcome.
    fn finish(&mut self, code: u8, msg: &str) -> u8 {
        if self.link_down {
            self.link_up();
        }
        if self.detached {
            self.reattach();
        }
        self.restore_nodes();
        for s in &self.stopped {
            self.ops.run(&["systemctl", "start", s]);
        }
        if self.stopped.contains(&"cardwired.service") {
            self.ops.sleep(Duration::from_secs(1));
            self.ops.run(&["cardwire", "debug", "refresh-gpu"]);
        }
        let _ = fs::write(&self.paths.live_result, format!("{msg}\n"));
        for f in [&self.paths.live_result, &self.paths.live_progress] {
            let _ = fs::set_permissions(f, fs::Permissions::from_mode(0o644));
        }
        self.record(msg);
        code
    }

    fn setpci_installed(&self) -> bool {
        self.ops.run(&["setpci", "--version"]).is_some()
    }

    fn switch(&mut self, target: &str) -> Result<Finish, Finish> {
        let paths = self.paths;
        let ops = self.ops;

        // --- checks that change nothing ----------------------------------------------------------
        let want_egpu = match target {
            "Hybrid" => "0",
            "AsusEgpu" => "1",
            _ => return Err(fin(2, format!("Unknown mode: '{target}'"))),
        };
        match try_lock(&paths.switch_lock) {
            Ok(Some(lock)) => self._lock = Some(lock),
            Ok(None) => return Err(fin(1, "Another GPU switch is already running")),
            Err(e) => return Err(fin(1, format!("Cannot open {}: {e}", paths.switch_lock.display()))),
        }
        if !paths.attr.join("egpu_enable/current_value").exists() {
            return Err(fin(1, "No asus-armoury egpu_enable attribute"));
        }
        if want_egpu == "1" && paths.read_attr("egpu_connected") != "1" {
            return Err(fin(3, "XG Mobile is not connected and locked"));
        }
        // After a GPU loss the NVIDIA driver is wedged: unbinding or rescanning would hang in the kernel.
        if ops.kernel_lost_gpu() {
            return Err(fin(1, "The NVIDIA driver lost a GPU in this boot - only a reboot gets out of this"));
        }
        if paths.read_attr("egpu_enable") == want_egpu {
            return self.already(target, want_egpu);
        }
        // Unlocked while egpu_enable is still 1: the firmware removes the XG Mobile's GPU within
        // 0.1-1.1 s and, when the driver could let go of it, switches back by itself (handled above
        // as "already").
        if paths.read_attr("egpu_enable") == "1" && paths.read_attr("egpu_connected") != "1" {
            return Err(fin(1, "The XG Mobile was unlocked while in use - its GPU is being removed; reboot instead"));
        }
        if paths.pending.exists() {
            return Err(fin(1, "A reboot-based switch is already scheduled"));
        }
        if paths.read_attr("gpu_mux_mode") == "0" {
            return Err(fin(1, "MUX mode is active (the dGPU drives the panel) - switch with a reboot"));
        }
        if !self.setpci_installed() {
            return Err(fin(1, "setpci (pciutils) is required for switching without a reboot"));
        }

        // Stopping cardwired also lifts its blocks, so a GPU blocked in Integrated/Smart mode shows
        // up in sysfs again; cardwired restores its mode when finish() starts it again.
        self.step(&format!("Live switch to {target}: stopping services"))?;
        for s in SERVICES {
            if ops.run(&["systemctl", "-q", "is-active", s]).is_some() && ops.run(&["systemctl", "stop", s]).is_some() {
                self.stopped.push(s);
            }
        }
        self.check()?;

        self.devices = nvidia_devices(paths);
        if self.devices.is_empty() {
            return Err(fin(1, "No NVIDIA card found"));
        }
        let parents: BTreeSet<PathBuf> = self
            .devices
            .iter()
            .map(|d| fs::canonicalize(d).ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default())
            .collect();
        if parents.len() != 1 {
            return Err(fin(1, "NVIDIA functions sit behind different ports - not supported"));
        }
        self.root = parents.first().map(|p| file_name(p)).unwrap_or_default();
        if !valid_bdf(&self.root) {
            return Err(fin(1, "Could not find the PCIe port of the NVIDIA card"));
        }
        // for boot-time switches and for bringing the dGPU back
        let _ = fs::write(paths.root_port_file(), format!("{}\n", self.root));
        let reset = paths.pci.join(&self.root).join("reset_subordinate");
        if !reset.exists() {
            return Err(fin(
                1,
                format!(
                    "The kernel cannot reset the bus below {} (no reset_subordinate) - switch with a reboot",
                    self.root
                ),
            ));
        }
        if !self.port().controllable() {
            return Err(fin(
                1,
                format!("Port {} has no PCIe capability that can be controlled - switch with a reboot", self.root),
            ));
        }
        self.uemsk = self.port().aer_mask(); // no AER on this port: skip the mask

        // --- keep new processes away from the card, then make sure nobody holds it ---------------
        let nodes = nvidia_nodes(paths);
        let mut sorted: Vec<&String> = nodes.iter().collect();
        sorted.sort();
        for n in sorted {
            let node = PathBuf::from(n);
            if !is_node(&node) {
                continue;
            }
            if let Ok(m) = fs::metadata(&node) {
                self.node_modes.push((node.clone(), m.mode() & 0o7777, m.rdev()));
                let _ = fs::set_permissions(&node, fs::Permissions::from_mode(0o000));
            }
        }
        let holders = holders(Path::new("/proc"), &nodes);
        if !holders.is_empty() {
            return Err(fin(4, format!("Aborted, the NVIDIA card is still in use by: {}", holders.join(", "))));
        }

        // --- unplug the card ---------------------------------------------------------------------
        self.step("Nobody holds the card; unbinding drivers")?;
        self.detached = true;
        for d in self.devices.clone() {
            if !d.join("driver").exists() {
                continue;
            }
            let addr = file_name(&d);
            self.step(&format!("unbind {addr} from {}", driver(&d)))?;
            if fs::write(d.join("driver/unbind"), &addr).is_err() {
                return Err(fin(5, format!("Unbind of {addr} failed")));
            }
        }
        self.step(&format!("secondary bus reset below {}", self.root))?;
        if fs::write(&reset, "1").is_err() {
            return Err(fin(
                8,
                format!("The bus reset below {} failed - aborted before the firmware switch", self.root),
            ));
        }
        self.pause(Duration::from_secs(1))?;
        for d in self.devices.clone() {
            self.step(&format!("remove {}", file_name(&d)))?;
            let _ = fs::write(d.join("remove"), "1");
        }

        self.link_down = true;
        let mut masked = "no AER on the port";
        if self.uemsk.is_some() {
            let port = self.port();
            port.set_aer_mask("00000020:00000020");
            let set = port.aer_mask().and_then(|v| u32::from_str_radix(&v, 16).ok()).is_some_and(|v| v & 0x20 != 0);
            masked = if set { "Surprise Down masked" } else { "Surprise Down mask not supported" };
        }
        self.port().disable_link();
        self.step(&format!("link down/up cycle on {} ({masked})", self.root))?;
        self.pause(Duration::from_millis(500))?;

        // --- the firmware switch -----------------------------------------------------------------
        self.step(&format!("egpu_enable -> {want_egpu}"))?;
        if fs::write(paths.attr.join("egpu_enable/current_value"), want_egpu).is_err() {
            self.record("writing egpu_enable failed");
        }
        let dgpu_disable = paths.attr.join("dgpu_disable/current_value");
        if want_egpu == "0" && dgpu_disable.exists() {
            let _ = fs::write(&dgpu_disable, "0"); // as in the tested sequence, also when already 0
        }
        let now_egpu = paths.read_attr("egpu_enable");
        self.pause(Duration::from_secs(2))?;

        self.link_up();
        self.reattach();

        let mut bound = None;
        for _ in 0..40 {
            bound = nvidia_bound(paths);
            if bound.is_some() {
                break;
            }
            self.pause(Duration::from_millis(500))?;
        }

        if now_egpu != want_egpu {
            return Err(fin(6, format!("The firmware did not switch (egpu_enable is still {now_egpu})")));
        }
        // udev runs this for the new card as well, but possibly only after finish() has started
        // cardwired again, and cardwired hides a blocked card's sysfs files from root too.
        for msg in egpu_power::run(paths) {
            ops.say(&msg);
        }
        if let Err(e) = set_supergfxd_mode(paths, target) {
            self.record(&format!("Updating {} failed: {e}", paths.supergfxd_conf.display()));
        }
        match bound {
            None => {
                Err(fin(6, format!("Switched egpu_enable to {want_egpu}, but no NVIDIA card got the driver - reboot")))
            }
            Some(d) => Ok(fin(0, format!("Switched to {target} without a reboot ({})", file_name(&d)))),
        }
    }

    /// Already in the target mode.
    fn already(&mut self, target: &str, want_egpu: &str) -> Result<Finish, Finish> {
        let paths = self.paths;
        // When the XG Mobile is unlocked while nothing holds its GPU, the firmware switches back to
        // the built-in dGPU by itself (as on Windows), but the root port's link stays disabled and
        // the dGPU never shows up. Bring the link up and rescan, as at the end of a live switch.
        let root = read(&paths.root_port_file());
        if want_egpu == "0"
            && nvidia_devices(paths).is_empty()
            && paths.read_attr("dgpu_disable") != "1"
            && valid_bdf(&root)
            && self.setpci_installed()
        {
            self.root = root;
            self.step(&format!(
                "Already in {target}, but the built-in dGPU is missing - bringing the link on {} back",
                self.root
            ))?;
            self.link_up();
            self.reattach();
            for _ in 0..40 {
                if let Some(d) = nvidia_bound(paths) {
                    return Ok(fin(0, format!("Built-in dGPU is back ({})", file_name(&d))));
                }
                self.pause(Duration::from_millis(500))?;
            }
            return Err(fin(6, "The built-in dGPU did not come back - reboot"));
        }
        Ok(fin(0, format!("Already in {target}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::testutil::{attr, FakeOps};
    use std::os::unix::fs::symlink;

    const PORT: &str = "0000:00:01.1";

    /// The X16 in Hybrid with the dGPU (GPU + audio) behind PORT, laid out like /sys/devices.
    fn setup() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = Paths::under(root);
        fs::create_dir_all(paths.switch_lock.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.state_dir()).unwrap();
        fs::create_dir_all(&paths.pci).unwrap();
        attr(&paths, "egpu_enable", "0");
        attr(&paths, "egpu_connected", "1");
        attr(&paths, "dgpu_disable", "0");
        attr(&paths, "gpu_mux_mode", "1");
        let port_dir = root.join("devices/pci0000:00").join(PORT);
        fs::create_dir_all(&port_dir).unwrap();
        fs::write(port_dir.join("reset_subordinate"), "").unwrap();
        symlink(&port_dir, paths.pci.join(PORT)).unwrap();
        let nvidia = root.join("drivers/nvidia");
        let hda = root.join("drivers/snd_hda_intel");
        fs::create_dir_all(&nvidia).unwrap();
        fs::create_dir_all(&hda).unwrap();
        for (func, drv) in [("0000:01:00.0", &nvidia), ("0000:01:00.1", &hda)] {
            let dir = port_dir.join(func);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("vendor"), "0x10de\n").unwrap();
            symlink(drv, dir.join("driver")).unwrap();
            symlink(&dir, paths.pci.join(func)).unwrap();
        }
        // The GPU's DRM node, readable by everyone.
        fs::create_dir_all(paths.pci.join("0000:01:00.0/drm/card1")).unwrap();
        fs::create_dir_all(paths.dev.join("dri")).unwrap();
        fs::write(paths.dev.join("dri/card1"), "").unwrap();
        fs::set_permissions(paths.dev.join("dri/card1"), fs::Permissions::from_mode(0o666)).unwrap();
        (tmp, paths)
    }

    /// setpci present, the port has PCIe and AER, the link comes up; the services are running.
    fn machine() -> FakeOps {
        let ops = FakeOps::default();
        ops.answer("setpci --version", Some("setpci version 3.15.0"));
        ops.answer(&format!("setpci -s {PORT} CAP_EXP+0x10.w="), Some(""));
        ops.answer(&format!("setpci -s {PORT} CAP_EXP+0x10.w"), Some("0040"));
        ops.answer(&format!("setpci -s {PORT} CAP_EXP+0x12.w"), Some("7103"));
        ops.answer(&format!("setpci -s {PORT} ECAP_AER+0x08.l="), Some(""));
        ops.answer(&format!("setpci -s {PORT} ECAP_AER+0x04.l="), Some(""));
        ops.answer(&format!("setpci -s {PORT} ECAP_AER+0x08.l"), Some("00400000"));
        ops.answer("systemctl -q is-active cardwired", Some(""));
        ops.answer("systemctl -q is-active nvidia-powerd", Some(""));
        ops.answer("systemctl stop", Some(""));
        ops.answer("systemctl start", Some(""));
        ops
    }

    fn steps(paths: &Paths) -> Vec<String> {
        fs::read_to_string(&paths.live_progress)
            .unwrap()
            .lines()
            .map(|l| l.split_once(' ').map_or(l, |(_, s)| s).to_string())
            .collect()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn switches_to_the_xg_mobile() {
        let (_tmp, paths) = setup();
        let ops = machine();
        assert_eq!(run("AsusEgpu", &paths, &ops), 0);
        assert_eq!(paths.read_attr("egpu_enable"), "1");
        assert_eq!(read(&paths.live_result), "Switched to AsusEgpu without a reboot (0000:01:00.0)");
        assert_eq!(read(&paths.root_port_file()), PORT);
        assert_eq!(
            steps(&paths),
            [
                "Live switch to AsusEgpu: stopping services",
                "Nobody holds the card; unbinding drivers",
                "unbind 0000:01:00.0 from nvidia",
                "unbind 0000:01:00.1 from snd_hda_intel",
                "secondary bus reset below 0000:00:01.1",
                "remove 0000:01:00.0",
                "remove 0000:01:00.1",
                "link down/up cycle on 0000:00:01.1 (Surprise Down mask not supported)",
                "egpu_enable -> 1",
                "enable link on 0000:00:01.1",
                "Rescanning PCI",
                "Switched to AsusEgpu without a reboot (0000:01:00.0)",
            ]
        );
        let cmds = ops.commands();
        let setpci: Vec<&str> = cmds.iter().filter_map(|c| c.strip_prefix(&format!("setpci -s {PORT} "))).collect();
        assert_eq!(
            setpci,
            [
                "CAP_EXP+0x10.w",
                "ECAP_AER+0x08.l",
                "ECAP_AER+0x08.l=00000020:00000020",
                "ECAP_AER+0x08.l",
                "CAP_EXP+0x10.w=0010:0010",
                "CAP_EXP+0x10.w=0000:0010",
                "CAP_EXP+0x12.w",
                "ECAP_AER+0x04.l=ffffffff",
                "ECAP_AER+0x08.l=00400000",
            ]
        );
        // Only the running services are stopped, and they are started again, cardwired refreshed.
        let systemctl: Vec<&String> =
            cmds.iter().filter(|c| c.starts_with("systemctl stop") || c.starts_with("systemctl start")).collect();
        assert_eq!(
            systemctl,
            [
                "systemctl stop cardwired.service",
                "systemctl stop nvidia-powerd.service",
                "systemctl start cardwired.service",
                "systemctl start nvidia-powerd.service",
            ]
        );
        assert_eq!(cmds.last().unwrap(), "cardwire debug refresh-gpu");
        assert_eq!(mode(&paths.dev.join("dri/card1")), 0o666, "node restored");
        assert_eq!(mode(&paths.live_result), 0o644);
    }

    #[test]
    fn interrupted_before_the_firmware_call_puts_everything_back() {
        let (_tmp, paths) = setup();
        let ops = FakeOps { interrupt_on: Some("egpu_enable ->".into()), ..machine() };
        assert_eq!(run("AsusEgpu", &paths, &ops), 7);
        assert_eq!(paths.read_attr("egpu_enable"), "0", "firmware not touched");
        assert_eq!(read(&paths.live_result), INTERRUPTED);
        let s = steps(&paths);
        assert_eq!(
            &s[s.len() - 4..],
            ["egpu_enable -> 1", "enable link on 0000:00:01.1", "Rescanning PCI", INTERRUPTED]
        );
        assert!(ops.commands().contains(&"systemctl start cardwired.service".to_string()));
        assert_eq!(mode(&paths.dev.join("dri/card1")), 0o666);
    }

    #[test]
    fn failed_unbind_reattaches_without_touching_the_link() {
        let (_tmp, paths) = setup();
        // Writing the unbind file fails when it is a directory.
        fs::create_dir_all(paths.pci.join("0000:01:00.0/driver/unbind")).unwrap();
        let ops = machine();
        assert_eq!(run("AsusEgpu", &paths, &ops), 5);
        assert_eq!(read(&paths.live_result), "Unbind of 0000:01:00.0 failed");
        let s = steps(&paths);
        assert!(s.contains(&"Rescanning PCI".to_string()));
        assert!(!s.iter().any(|l| l.starts_with("enable link")));
        assert_eq!(paths.read_attr("egpu_enable"), "0");
    }

    #[test]
    fn refusals_change_nothing() {
        type Case = (&'static str, fn(&Paths, &FakeOps), u8, &'static str);
        let cases: [Case; 8] = [
            ("Bogus", |_, _| {}, 2, "Unknown mode: 'Bogus'"),
            ("AsusEgpu", |p, _| attr(p, "egpu_connected", "0"), 3, "XG Mobile is not connected and locked"),
            ("Hybrid", |_, _| {}, 0, "Already in Hybrid"),
            (
                "AsusEgpu",
                |p, _| fs::write(&p.pending, "Hybrid").unwrap(),
                1,
                "A reboot-based switch is already scheduled",
            ),
            (
                "AsusEgpu",
                |p, _| attr(p, "gpu_mux_mode", "0"),
                1,
                "MUX mode is active (the dGPU drives the panel) - switch with a reboot",
            ),
            (
                "AsusEgpu",
                |_, o| o.answers.borrow_mut().retain(|(a, _)| !a.starts_with("setpci --version")),
                1,
                "setpci (pciutils) is required for switching without a reboot",
            ),
            (
                "Hybrid",
                |p, _| {
                    attr(p, "egpu_enable", "1");
                    attr(p, "egpu_connected", "0")
                },
                1,
                "The XG Mobile was unlocked while in use - its GPU is being removed; reboot instead",
            ),
            (
                "AsusEgpu",
                |p, _| fs::remove_file(p.pci.join(PORT).join("reset_subordinate")).unwrap(),
                1,
                "The kernel cannot reset the bus below 0000:00:01.1 (no reset_subordinate) - switch with a reboot",
            ),
        ];
        for (target, prepare, code, msg) in cases {
            let (_tmp, paths) = setup();
            let ops = machine();
            prepare(&paths, &ops);
            assert_eq!(run(target, &paths, &ops), code, "{msg}");
            assert_eq!(read(&paths.live_result), msg);
            assert!(!ops.commands().iter().any(|c| c.contains("=0010:0010")), "{msg}: link touched");
            assert!(paths.pci.join("0000:01:00.0/driver").exists());
        }

        let (_tmp, paths) = setup();
        let ops = FakeOps { kernel_lost: true, ..machine() };
        assert_eq!(run("AsusEgpu", &paths, &ops), 1);
        assert!(ops.commands().iter().all(|c| !c.starts_with("systemctl stop")));
    }

    #[test]
    fn holders_found_by_open_descriptor() {
        let tmp = tempfile::tempdir().unwrap();
        let node = tmp.path().join("card1");
        fs::write(&node, "").unwrap();
        let proc = tmp.path().join("proc");
        for (pid, comm, target) in [("100", "kwin_wayland", &node), ("200", "bash", &tmp.path().join("other"))] {
            fs::create_dir_all(proc.join(pid).join("fd")).unwrap();
            fs::write(proc.join(pid).join("comm"), format!("{comm}\n")).unwrap();
            symlink(target, proc.join(pid).join("fd/3")).unwrap();
        }
        fs::create_dir_all(proc.join("self/fd")).unwrap();
        let nodes = HashSet::from([node.to_string_lossy().into_owned()]);
        assert_eq!(holders(&proc, &nodes), ["kwin_wayland (100)"]);
        assert!(holders(&proc, &HashSet::new()).is_empty());
    }

    #[test]
    fn lock_held() {
        let (_tmp, paths) = setup();
        let _held = try_lock(&paths.switch_lock).unwrap().unwrap();
        assert_eq!(run("AsusEgpu", &paths, &machine()), 1);
        assert_eq!(read(&paths.live_result), "Another GPU switch is already running");
    }

    #[test]
    fn missing_dgpu_is_brought_back() {
        let (_tmp, paths) = setup();
        for f in ["0000:01:00.0", "0000:01:00.1"] {
            fs::remove_file(paths.pci.join(f)).unwrap();
        }
        fs::write(paths.root_port_file(), format!("{PORT}\n")).unwrap();
        let ops = machine();
        assert_eq!(run("Hybrid", &paths, &ops), 6, "the fake rescan brings nothing back");
        assert_eq!(
            steps(&paths),
            [
                "Already in Hybrid, but the built-in dGPU is missing - bringing the link on 0000:00:01.1 back",
                "enable link on 0000:00:01.1",
                "Rescanning PCI",
                "The built-in dGPU did not come back - reboot",
            ]
        );
        assert_eq!(read(&paths.pci.parent().unwrap().join("rescan")), "1");
    }
}

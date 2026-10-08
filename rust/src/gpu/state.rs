//! The whole GPU state as the menu and `dump` show it.

use std::time::{Duration, Instant};

use super::cardwire::{self, Cardwire, CwDevices};
use super::cmd::Cmd;
use super::paths::{read, sorted_entries, Paths, LIVE_UNIT, REBOOT_UNIT};
use super::pci::{detect_gpus, Gpu, Kind};

pub const REBOOT_MODES: [&str; 4] = ["Integrated", "Hybrid", "AsusEgpu", "AsusMuxDgpu"];
/// Built-in dGPU <-> XG Mobile switch without a reboot.
pub const LIVE_MODES: [&str; 2] = ["Hybrid", "AsusEgpu"];
pub const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// cardwired restarts per episode when refresh-gpu does not fix the GPU list.
pub const CW_RESTARTS_MAX: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuState {
    pub gpus: Vec<Gpu>,
    pub cardwire: bool,
    /// lower case, e.g. "hybrid"
    pub cw_mode: String,
    pub cw_modes: Vec<String>,
    /// egpu_connected attribute exists (ASUS XG Mobile)
    pub asus_egpu: bool,
    pub egpu_connected: bool,
    /// "Hybrid" (built-in dGPU) / "AsusEgpu" / "AsusMuxDgpu" / "" when not ASUS
    pub hw_mode: String,
    pub has_mux: bool,
    /// hardware mode scheduled for the next boot
    pub hw_pending: String,
    pub reboot_backend: bool,
    pub live_backend: bool,
    /// asus-armoury dgpu_disable: the built-in dGPU is powered off on purpose
    pub dgpu_disabled: bool,
}

impl GpuState {
    fn of_kind(&self, kind: Kind) -> Option<&Gpu> {
        self.gpus.iter().find(|g| g.kind == kind)
    }

    pub fn igpu(&self) -> Option<&Gpu> {
        self.of_kind(Kind::Igpu)
    }

    pub fn dgpu(&self) -> Option<&Gpu> {
        self.of_kind(Kind::Dgpu)
    }

    pub fn egpu(&self) -> Option<&Gpu> {
        self.of_kind(Kind::Egpu)
    }
}

/// Repair of a cardwired that missed the dGPU: refresh-gpu first; it did not always help
/// (2026-10-04), restarting cardwired did. The polkit rule allows exactly this restart.
#[derive(Debug, Default)]
pub struct CwRepair {
    last: Option<Instant>,
    count: u32,
}

impl CwRepair {
    fn due(&self) -> bool {
        self.last.is_none_or(|t| t.elapsed() > REFRESH_EVERY)
    }

    /// Returns the devices to use: re-read after a repair attempt.
    fn check(
        &mut self,
        paths: &Paths,
        cardwire: &dyn Cardwire,
        cmd: &dyn Cmd,
        cw: Option<CwDevices>,
    ) -> Option<CwDevices> {
        let missed = cw.as_ref().is_some_and(|c| !c.is_empty() && cardwire::missed_dgpu(paths, c));
        if !missed {
            self.count = 0;
            return cw;
        }
        if !self.due() {
            return cw;
        }
        self.last = Some(Instant::now());
        if self.count == 0 {
            cardwire.refresh_gpu();
        } else if self.count <= CW_RESTARTS_MAX {
            cmd.run(&["systemctl", "--no-block", "restart", "cardwired.service"]);
        }
        self.count += 1;
        cardwire.devices()
    }

    #[cfg(test)]
    pub(crate) fn make_due(&mut self) {
        self.last = None;
    }
}

pub fn read_state(paths: &Paths, cardwire: &dyn Cardwire, cmd: &dyn Cmd, repair: &mut CwRepair) -> GpuState {
    let cw = repair.check(paths, cardwire, cmd, cardwire.devices());
    let (cw_mode, cw_modes) = if cw.is_some() { cardwire.mode() } else { Default::default() };
    let asus_egpu = paths.attr.join("egpu_connected").exists();
    let hw_mode = if !asus_egpu {
        ""
    } else if paths.read_attr("egpu_enable") == "1" {
        "AsusEgpu"
    } else if paths.read_attr("gpu_mux_mode") == "0" {
        "AsusMuxDgpu"
    } else {
        "Hybrid"
    };
    GpuState {
        gpus: detect_gpus(paths, &cw.unwrap_or_default()),
        cardwire: !cw_mode.is_empty(),
        cw_mode,
        cw_modes,
        asus_egpu,
        egpu_connected: paths.read_attr("egpu_connected") == "1",
        hw_mode: hw_mode.into(),
        has_mux: paths.attr.join("gpu_mux_mode").exists(),
        hw_pending: read(&paths.pending),
        reboot_backend: paths.unit_installed(REBOOT_UNIT) && asus_egpu,
        live_backend: paths.unit_installed(LIVE_UNIT) && asus_egpu,
        dgpu_disabled: paths.read_attr("dgpu_disable") == "1",
    }
}

/// The card currently rendering (shown by the icon).
pub fn working_gpu(s: &GpuState) -> Option<&Gpu> {
    for g in [s.egpu(), s.dgpu()].into_iter().flatten() {
        if !g.driver.is_empty() && !g.blocked && g.power == "active" {
            return Some(g);
        }
    }
    if s.hw_mode == "AsusMuxDgpu" {
        return s.dgpu().or(s.igpu()); // with the MUX the dGPU drives the panel
    }
    s.igpu().or(s.gpus.first())
}

/// Built-in dGPU <-> XG Mobile can switch live; anything involving the MUX needs a reboot.
pub fn can_switch_live(s: &GpuState, mode: &str) -> bool {
    s.live_backend && s.hw_pending.is_empty() && LIVE_MODES.contains(&mode) && LIVE_MODES.contains(&s.hw_mode.as_str())
}

pub fn hw_modes(s: &GpuState) -> Vec<&'static str> {
    match (s.asus_egpu, s.has_mux) {
        (false, _) => vec![],
        (true, false) => vec!["Hybrid", "AsusEgpu"],
        (true, true) => vec!["Hybrid", "AsusEgpu", "AsusMuxDgpu"],
    }
}

/// The XG Mobile is still the active GPU, but its lock was opened (or the cable pulled).
pub fn xg_unlocked(s: &GpuState) -> bool {
    s.hw_mode == "AsusEgpu" && !s.egpu_connected && s.hw_pending.is_empty()
}

/// Built-in dGPU mode, but the dGPU is not on the bus. What a clean undock leaves behind: with
/// nothing holding the XG Mobile's GPU, the firmware switches back by itself (as on Windows) but
/// the root port's link stays disabled. asus-gpu-live@Hybrid brings it back.
pub fn dgpu_missing(s: &GpuState) -> bool {
    s.asus_egpu && s.hw_mode == "Hybrid" && s.dgpu().is_none() && !s.dgpu_disabled && s.hw_pending.is_empty()
}

/// XG Mobile mode, but its GPU is no longer on the bus. Unlocking the XG Mobile makes the
/// firmware drop the GPU at once (GV601RE: ~0.1 s after the lock event), and it does not come back
/// when the dock is connected again; only a reboot recovers it.
pub fn xg_gone(s: &GpuState) -> bool {
    s.hw_mode == "AsusEgpu" && s.egpu().is_none() && s.hw_pending.is_empty()
}

pub fn on_battery(paths: &Paths) -> bool {
    let mains: Vec<_> =
        sorted_entries(&paths.power_supply).into_iter().filter(|p| read(&p.join("type")) == "Mains").collect();
    !mains.is_empty() && !mains.iter().any(|p| read(&p.join("online")) == "1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::testutil::{egpu, igpu, state, FakeCmd};

    #[test]
    fn test_can_switch_live() {
        assert!(can_switch_live(&state(), "Hybrid"));
        assert!(!can_switch_live(&state(), "AsusMuxDgpu"));
        assert!(!can_switch_live(&GpuState { hw_mode: "AsusMuxDgpu".into(), ..state() }, "Hybrid"));
        assert!(!can_switch_live(&GpuState { hw_pending: "Hybrid".into(), ..state() }, "Hybrid"));
        assert!(!can_switch_live(&GpuState { live_backend: false, ..state() }, "Hybrid"));
    }

    #[test]
    fn working_gpu_ignores_sleeping_and_blocked_cards() {
        assert_eq!(working_gpu(&state()), Some(&igpu()));
        let awake = Gpu { power: "active".into(), ..egpu() };
        assert_eq!(working_gpu(&GpuState { gpus: vec![awake.clone(), igpu()], ..state() }), Some(&awake));
        let blocked = Gpu { power: "active".into(), blocked: true, ..egpu() };
        assert_eq!(working_gpu(&GpuState { gpus: vec![blocked, igpu()], ..state() }), Some(&igpu()));
    }

    #[test]
    fn xg_states() {
        assert!(xg_unlocked(&GpuState { egpu_connected: false, ..state() }));
        assert!(!xg_unlocked(&state()));
        assert!(xg_gone(&GpuState { gpus: vec![igpu()], ..state() }));
        assert!(dgpu_missing(&GpuState { gpus: vec![igpu()], hw_mode: "Hybrid".into(), ..state() }));
        assert!(!dgpu_missing(&GpuState {
            gpus: vec![igpu()],
            hw_mode: "Hybrid".into(),
            dgpu_disabled: true,
            ..state()
        }));
    }

    #[test]
    fn refresh_then_restart_then_give_up() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        crate::gpu::testutil::write(&paths.pci.join("0000:01:00.0/class"), "0x030000");
        crate::gpu::testutil::write(&paths.pci.join("0000:01:00.0/vendor"), "0x10de");
        std::fs::create_dir_all(tmp.path().join("drivers/nvidia")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("drivers/nvidia"), paths.pci.join("0000:01:00.0/driver")).unwrap();
        // cardwire took the dGPU for an integrated GPU
        let cmd = FakeCmd::new(&[]).cardwire(&[("0000:01:00.0", false)]);
        let mut repair = CwRepair::default();
        for _ in 0..5 {
            repair.make_due();
            read_state(&paths, &cmd, &cmd, &mut repair);
        }
        let repairs: Vec<String> = cmd
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("cardwire refresh-gpu") || c.starts_with("systemctl"))
            .collect();
        assert_eq!(repairs[0], "cardwire refresh-gpu");
        assert_eq!(repairs[1..], vec!["systemctl --no-block restart cardwired.service"; CW_RESTARTS_MAX as usize]);
    }
}

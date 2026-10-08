//! Test helpers: the same fixed GPUs and state as tests/test_tray.py, a fake command runner.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::cardwire::{Cardwire, CwDevice, CwDevices};
use super::cmd::Cmd;
use super::pci::{Gpu, Kind};
use super::state::GpuState;

pub fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn gpu(addr: &str, vendor: &str, name: &str, driver: &str, kind: Kind, power: &str) -> Gpu {
    Gpu {
        addr: addr.into(),
        vendor: vendor.into(),
        name: name.into(),
        driver: driver.into(),
        kind,
        power: power.into(),
        blocked: false,
    }
}

pub fn igpu() -> Gpu {
    gpu("0000:3a:00.0", "AMD", "Radeon 680M", "amdgpu", Kind::Igpu, "active")
}

pub fn egpu() -> Gpu {
    gpu("0000:01:00.0", "NVIDIA", "RTX 3070", "nvidia", Kind::Egpu, "suspended")
}

pub fn dgpu() -> Gpu {
    gpu("0000:01:00.0", "NVIDIA", "RTX 3050 Ti", "nvidia", Kind::Dgpu, "suspended")
}

/// XG Mobile connected and active, cardwire in hybrid mode.
pub fn state() -> GpuState {
    GpuState {
        gpus: vec![egpu(), igpu()],
        cardwire: true,
        cw_mode: "hybrid".into(),
        cw_modes: vec!["integrated".into(), "hybrid".into(), "smart".into()],
        asus_egpu: true,
        egpu_connected: true,
        hw_mode: "AsusEgpu".into(),
        has_mux: true,
        hw_pending: String::new(),
        reboot_backend: true,
        live_backend: true,
        dgpu_disabled: false,
    }
}

/// Answers from a table (keyed by the command line joined with spaces) and records every call.
/// Also stands in for cardwired; its repair call is recorded as "cardwire refresh-gpu".
#[derive(Default)]
pub struct FakeCmd {
    installed: Vec<String>,
    outputs: HashMap<String, String>,
    calls: RefCell<Vec<String>>,
    cw_devices: Option<CwDevices>,
    cw_mode: (String, Vec<String>),
}

impl FakeCmd {
    pub fn new(installed: &[&str]) -> Self {
        FakeCmd { installed: installed.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    pub fn out(mut self, cmdline: &str, output: &str) -> Self {
        self.outputs.insert(cmdline.into(), output.into());
        self
    }

    /// cardwired running with these GPUs: (PCI address, discrete).
    pub fn cardwire(mut self, gpus: &[(&str, bool)]) -> Self {
        let devices = gpus
            .iter()
            .map(|(addr, discrete)| (addr.to_string(), CwDevice { discrete: *discrete, ..Default::default() }));
        self.cw_devices = Some(devices.collect());
        self
    }

    pub fn cardwire_devices(mut self, devices: CwDevices, mode: &str, modes: &[&str]) -> Self {
        self.cw_devices = Some(devices);
        self.cw_mode = (mode.into(), modes.iter().map(|m| m.to_string()).collect());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Cmd for FakeCmd {
    fn run(&self, args: &[&str]) -> String {
        let line = args.join(" ");
        self.calls.borrow_mut().push(line.clone());
        self.outputs.get(&line).cloned().unwrap_or_default()
    }

    fn which(&self, name: &str) -> bool {
        self.installed.iter().any(|n| n == name)
    }
}

impl Cardwire for FakeCmd {
    fn devices(&self) -> Option<CwDevices> {
        self.cw_devices.clone()
    }

    fn mode(&self) -> (String, Vec<String>) {
        self.cw_mode.clone()
    }

    fn refresh_gpu(&self) {
        self.calls.borrow_mut().push("cardwire refresh-gpu".into());
    }
}

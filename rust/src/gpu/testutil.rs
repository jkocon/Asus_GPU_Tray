//! Test helpers: the same fixed GPUs and state as tests/test_tray.py, a fake command runner.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

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
#[derive(Default)]
pub struct FakeCmd {
    installed: Vec<String>,
    outputs: HashMap<String, String>,
    calls: RefCell<Vec<String>>,
}

impl FakeCmd {
    pub fn new(installed: &[&str]) -> Self {
        FakeCmd { installed: installed.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    pub fn out(mut self, cmdline: &str, output: &str) -> Self {
        self.outputs.insert(cmdline.into(), output.into());
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

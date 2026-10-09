//! Where the program looks at the system. Tests point these at a fake tree.

use std::fs;
use std::path::{Path, PathBuf};

pub const LIVE_UNIT: &str = "asus-gpu-live@.service";
pub const REBOOT_UNIT: &str = "asus-gpu-switch@.service";

#[derive(Clone, Debug)]
pub struct Paths {
    pub pci: PathBuf,
    pub attr: PathBuf,
    pub pci_ids: PathBuf,
    pub unit_dirs: Vec<PathBuf>,
    pub pending: PathBuf,
    pub live_result: PathBuf,
    pub live_progress: PathBuf,
    pub power_supply: PathBuf,
    pub dev: PathBuf,
    /// Held by every hardware switch (root-only; /run/lock is world-writable on some distros).
    pub switch_lock: PathBuf,
    pub sysrq: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            pci: "/sys/bus/pci/devices".into(),
            attr: "/sys/class/firmware-attributes/asus-armoury/attributes".into(),
            pci_ids: "/usr/share/hwdata/pci.ids".into(),
            unit_dirs: vec!["/etc/systemd/system".into(), "/usr/lib/systemd/system".into()],
            pending: "/var/lib/asus-gpu-tray/pending".into(),
            live_result: "/var/lib/asus-gpu-tray/live-result".into(),
            live_progress: "/var/lib/asus-gpu-tray/live-progress".into(),
            power_supply: "/sys/class/power_supply".into(),
            dev: "/dev".into(),
            switch_lock: "/run/asus-gpu-tray.lock".into(),
            sysrq: "/proc/sysrq-trigger".into(),
        }
    }
}

impl Paths {
    /// Everything below one directory, laid out like the real system (for tests).
    pub fn under(root: &Path) -> Self {
        Paths {
            pci: root.join("pci"),
            attr: root.join("attr"),
            pci_ids: root.join("pci.ids"),
            unit_dirs: vec![root.join("units")],
            pending: root.join("state/pending"),
            live_result: root.join("state/live-result"),
            live_progress: root.join("state/live-progress"),
            power_supply: root.join("power_supply"),
            dev: root.join("dev"),
            switch_lock: root.join("run/asus-gpu-tray.lock"),
            sysrq: root.join("sysrq-trigger"),
        }
    }

    pub fn read_attr(&self, name: &str) -> String {
        read(&self.attr.join(name).join("current_value"))
    }

    /// /var/lib/asus-gpu-tray: pending, live-progress, live-result, root-port.
    pub fn state_dir(&self) -> PathBuf {
        self.pending.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    pub fn unit_installed(&self, name: &str) -> bool {
        self.unit_dirs.iter().any(|d| d.join(name).exists())
    }
}

/// File contents without surrounding whitespace; "" when the file cannot be read.
pub fn read(path: &Path) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// Directory entries sorted by name; empty when the directory cannot be read.
pub fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        fs::read_dir(dir).map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
    v.sort();
    v
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

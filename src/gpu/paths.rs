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
    /// Where the binary is installed: /usr/local/lib/asus-gpu-tray (install.sh) or
    /// /usr/lib/asus-gpu-tray (package).
    pub lib_dir: PathBuf,
    /// The text console switch-apply writes to while the login screen waits.
    pub console: PathBuf,
    /// One-boot NVIDIA blacklist left by older versions of switch-reboot.
    pub switch_blacklist: PathBuf,
    pub supergfxd_conf: PathBuf,
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
            lib_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| "/usr/local/lib/asus-gpu-tray".into()),
            console: "/dev/tty1".into(),
            switch_blacklist: "/etc/modprobe.d/zz-asus-gpu-tray-switch.conf".into(),
            supergfxd_conf: "/etc/supergfxd.conf".into(),
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
            lib_dir: root.join("lib"),
            console: root.join("tty1"),
            switch_blacklist: root.join("modprobe.d/zz-asus-gpu-tray-switch.conf"),
            supergfxd_conf: root.join("supergfxd.conf"),
        }
    }

    pub fn read_attr(&self, name: &str) -> String {
        read(&self.attr.join(name).join("current_value"))
    }

    /// /var/lib/asus-gpu-tray: pending, live-progress, live-result, root-port.
    pub fn state_dir(&self) -> PathBuf {
        self.pending.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    /// The NVIDIA card's root port, remembered by the switches for boots where no card is visible.
    pub fn root_port_file(&self) -> PathBuf {
        self.state_dir().join("root-port")
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

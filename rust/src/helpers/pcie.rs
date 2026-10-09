//! The NVIDIA card's PCI functions and its PCIe root port, shared by switch-apply and switch-live.
//! Port registers go through setpci, as in the scripts.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Ops;
use crate::gpu::paths::{file_name, read, sorted_entries, Paths};

/// Every NVIDIA PCI function (GPU and its HDMI audio).
pub fn nvidia_devices(paths: &Paths) -> Vec<PathBuf> {
    sorted_entries(&paths.pci).into_iter().filter(|d| read(&d.join("vendor")) == "0x10de").collect()
}

/// The name of the driver bound to a PCI function; "" when none.
pub fn driver(dev: &Path) -> String {
    fs::read_link(dev.join("driver")).map(|l| file_name(&l)).unwrap_or_default()
}

/// The (last) NVIDIA function the nvidia driver is bound to.
pub fn nvidia_bound(paths: &Paths) -> Option<PathBuf> {
    nvidia_devices(paths).into_iter().rfind(|d| driver(d) == "nvidia")
}

/// The PCI address of the port a function sits behind (the parent in /sys/devices).
pub fn parent_port(dev: &Path) -> String {
    fs::canonicalize(dev).ok().and_then(|p| p.parent().map(file_name)).unwrap_or_default()
}

/// A full PCI address, 0000:00:01.1.
pub fn valid_bdf(s: &str) -> bool {
    let b = s.as_bytes();
    let hex = |r: std::ops::Range<usize>| b[r].iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    b.len() == 12
        && hex(0..4)
        && b[4] == b':'
        && hex(5..7)
        && b[7] == b':'
        && hex(8..10)
        && b[10] == b'.'
        && (b'0'..=b'7').contains(&b[11])
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// /sys/bus/pci/rescan: brings back removed functions.
pub fn rescan(paths: &Paths) -> std::io::Result<()> {
    fs::write(bus_dir(paths).join("rescan"), "1")
}

/// /sys/bus/pci/drivers_probe: binds a driver to a function that has none.
pub fn probe(paths: &Paths, dev: &Path) {
    let _ = fs::write(bus_dir(paths).join("drivers_probe"), file_name(dev));
}

fn bus_dir(paths: &Paths) -> PathBuf {
    paths.pci.parent().map(Path::to_path_buf).unwrap_or_default()
}

pub struct Port<'a> {
    pub bdf: String,
    ops: &'a dyn Ops,
}

impl<'a> Port<'a> {
    pub fn new(bdf: &str, ops: &'a dyn Ops) -> Self {
        Port { bdf: bdf.to_string(), ops }
    }

    fn get(&self, reg: &str) -> Option<String> {
        self.ops.run(&["setpci", "-s", &self.bdf, reg])
    }

    fn set(&self, reg_value: &str) -> bool {
        self.ops.run(&["setpci", "-s", &self.bdf, reg_value]).is_some()
    }

    /// setpci is there and the port has a PCIe capability it can read.
    pub fn controllable(&self) -> bool {
        self.get("CAP_EXP+0x10.w").is_some_and(|v| is_hex(&v, 4))
    }

    /// Link Control: Link Disable on.
    pub fn disable_link(&self) {
        self.set("CAP_EXP+0x10.w=0010:0010");
    }

    /// Link Control: Link Disable off.
    pub fn enable_link(&self) {
        self.set("CAP_EXP+0x10.w=0000:0010");
    }

    /// Link Status: Data Link Layer Link Active.
    fn link_active(&self) -> bool {
        self.get("CAP_EXP+0x12.w").and_then(|v| u16::from_str_radix(&v, 16).ok()).is_some_and(|v| v & 0x2000 != 0)
    }

    /// Up to 20 s; the XG Mobile cable needs ~8 s.
    pub fn wait_link(&self) -> bool {
        for _ in 0..80 {
            if self.link_active() {
                return true;
            }
            self.ops.sleep(Duration::from_millis(250));
        }
        false
    }

    /// Clears the AER uncorrectable status (what the link change logged).
    pub fn clear_aer(&self) {
        self.set("ECAP_AER+0x04.l=ffffffff");
    }

    /// The AER uncorrectable error mask; None when the port has no AER.
    pub fn aer_mask(&self) -> Option<String> {
        self.get("ECAP_AER+0x08.l").filter(|v| is_hex(v, 8))
    }

    pub fn set_aer_mask(&self, value_mask: &str) {
        self.set(&format!("ECAP_AER+0x08.l={value_mask}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::testutil::FakeOps;

    #[test]
    fn bdf() {
        assert!(valid_bdf("0000:00:01.1"));
        for bad in ["", "0000:00:01.8", "0000:00:01", "0000:00:0g.1", "0000:00:01.1\n", "x0000:00:01.1"] {
            assert!(!valid_bdf(bad), "{bad:?}");
        }
    }

    #[test]
    fn link_state_from_setpci() {
        let ops = FakeOps::default();
        let port = Port::new("0000:00:01.1", &ops);
        assert!(!port.controllable(), "setpci missing");
        assert!(!port.wait_link());
        assert_eq!(ops.commands().len(), 81, "one probe, then 80 link reads");

        ops.answer("setpci -s 0000:00:01.1 CAP_EXP+0x10.w", Some("0040"));
        ops.answer("setpci -s 0000:00:01.1 CAP_EXP+0x12.w", Some("7103"));
        assert!(port.controllable());
        assert!(port.wait_link());
        assert_eq!(port.aer_mask(), None);
    }
}

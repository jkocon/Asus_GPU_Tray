//! cardwire: GPU access modes (Integrated / Hybrid / Smart). State is read over D-Bus
//! (see `cardwire_dbus`); the mode is changed with `cardwire set`, whose error messages the
//! tray shows as they are.

use std::collections::BTreeMap;

use super::paths::Paths;
use super::pci::{detect_gpus, Kind};

/// Order in the menu.
pub const MODES: [&str; 3] = ["integrated", "hybrid", "smart"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CwDevice {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub driver: String,
    pub blocked: bool,
    pub discrete: bool,
}

/// PCI address -> cardwire's device info.
pub type CwDevices = BTreeMap<String, CwDevice>;

/// What the core needs from cardwired.
pub trait Cardwire {
    /// None when cardwired is not running.
    fn devices(&self) -> Option<CwDevices>;
    /// Current mode and the available ones (lower case, menu order); ("", []) when unknown.
    fn mode(&self) -> (String, Vec<String>);
    /// Make cardwired enumerate the GPUs again.
    fn refresh_gpu(&self);
}

/// cardwired's mode number on D-Bus -> name (enum Modes in cardwire 0.12).
pub fn mode_name(n: u32) -> String {
    match n {
        0 => "integrated",
        1 => "hybrid",
        2 => "manual",
        3 => "smart",
        _ => return format!("mode {n}"),
    }
    .to_string()
}

/// Known modes in menu order, then any others in cardwired's order.
pub fn order_modes(modes: Vec<String>) -> Vec<String> {
    let known = MODES.iter().filter(|m| modes.iter().any(|x| x.as_str() == **m)).map(|m| m.to_string());
    let other = modes.iter().filter(|m| !MODES.contains(&m.as_str())).cloned();
    known.chain(other).collect()
}

/// "NVIDIA GeForce RTX 3070 Laptop GPU" -> "RTX 3070"
pub fn short_name(name: &str) -> String {
    let mut name = name.to_string();
    for word in ["NVIDIA ", "GeForce ", "AMD ", "Intel(R) ", "Intel ", " Laptop GPU", " Mobile"] {
        name = name.replace(word, "");
    }
    name.trim().to_string()
}

/// cardwired can start before the NVIDIA driver is ready and then mistake the dGPU for an
/// integrated one (the laptop looks like a desktop: only hybrid/manual modes).
pub fn missed_dgpu(paths: &Paths, cw: &CwDevices) -> bool {
    if cw.is_empty() || cw.values().any(|d| d.discrete) {
        return false;
    }
    detect_gpus(paths, cw).iter().any(|g| g.kind != Kind::Igpu && !g.driver.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        let modes = [1, 0, 2, 3].map(mode_name).to_vec();
        assert_eq!(order_modes(modes), ["integrated", "hybrid", "smart", "manual"]);
        assert_eq!(mode_name(7), "mode 7");
        assert_eq!(short_name("NVIDIA GeForce RTX 3050 Ti Laptop GPU"), "RTX 3050 Ti");
    }
}

//! `egpu-power`. Run as root by udev
//! (udev/72-asus-gpu-tray-egpu.rules) when an NVIDIA PCI function or the asus-armoury attributes
//! appear, and after a live switch.
//!
//! Keeps the XG Mobile GPU out of D3cold. Waking the XG Mobile's RTX 3070 from D3cold failed on a
//! GV601RE ("Unable to change power state from D3cold to D0, device inaccessible", Xid 79, the root
//! port retraining a "broken device"), and the GPU stayed lost until a reboot. With
//! d3cold_allowed = 0 the card can still runtime-suspend to D3hot, and its root port stays powered.
//! The dock has its own power supply, so this costs no battery. The built-in dGPU is left alone:
//! its functions are created anew on every switch, with the kernel default (D3cold allowed).

use std::fs;

use crate::gpu::paths::{file_name, read, sorted_entries, Paths};

/// Sets d3cold_allowed = 0 on every NVIDIA function while the XG Mobile is enabled. Returns one
/// message per function changed.
pub fn run(paths: &Paths) -> Vec<String> {
    if paths.read_attr("egpu_enable") != "1" {
        return Vec::new();
    }
    let mut changed = Vec::new();
    for d in sorted_entries(&paths.pci) {
        let knob = d.join("d3cold_allowed");
        if read(&d.join("vendor")) != "0x10de" || read(&knob) == "0" {
            continue;
        }
        // The script's `-w` test: a knob that cannot be written is skipped silently.
        if fs::write(&knob, "0").is_ok() {
            changed.push(format!("XG Mobile: D3cold disabled for {}", file_name(&d)));
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn function(root: &Path, addr: &str, vendor: &str, d3cold: &str) {
        let d = root.join("pci").join(addr);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
        fs::write(d.join("d3cold_allowed"), format!("{d3cold}\n")).unwrap();
    }

    fn egpu_enable(root: &Path, value: &str) {
        let a = root.join("attr/egpu_enable");
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("current_value"), value).unwrap();
    }

    #[test]
    fn only_nvidia_functions_in_xg_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        function(root, "0000:01:00.0", "0x10de", "1");
        function(root, "0000:01:00.1", "0x10de", "1");
        function(root, "0000:3a:00.0", "0x1002", "1");
        function(root, "0000:3b:00.0", "0x10de", "0"); // already off: no message
        let paths = Paths::under(root);

        egpu_enable(root, "0");
        assert!(run(&paths).is_empty());
        assert_eq!(read(&root.join("pci/0000:01:00.0/d3cold_allowed")), "1");

        egpu_enable(root, "1");
        assert_eq!(
            run(&paths),
            ["XG Mobile: D3cold disabled for 0000:01:00.0", "XG Mobile: D3cold disabled for 0000:01:00.1"]
        );
        assert_eq!(read(&root.join("pci/0000:01:00.0/d3cold_allowed")), "0");
        assert_eq!(read(&root.join("pci/0000:3a:00.0/d3cold_allowed")), "1");
        assert!(run(&paths).is_empty(), "second run changes nothing");
    }

    #[test]
    fn no_attributes_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        function(tmp.path(), "0000:01:00.0", "0x10de", "1");
        assert!(run(&Paths::under(tmp.path())).is_empty());
    }
}

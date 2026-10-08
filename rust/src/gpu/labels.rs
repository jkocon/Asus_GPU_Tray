//! Texts shown in the menu and printed by `dump`. The msgids are those of asus_gpu_tray.py.

use std::fmt::Write;

use super::i18n::{capitalize_first, tr, trf};
use super::paths::{read, Paths};
use super::pci::{Gpu, Kind};
use super::state::{hw_modes, working_gpu, xg_gone, xg_unlocked, GpuState};

/// Label for an ASUS hardware mode.
pub fn hw_label(mode: &str, s: &GpuState) -> String {
    match mode {
        "Hybrid" => match s.dgpu() {
            Some(g) => trf("Built-in dGPU ({name})", &[("name", &g.name)]),
            None => tr("Built-in dGPU"),
        },
        "AsusMuxDgpu" => {
            let dg = s.dgpu().map_or_else(|| tr("Built-in dGPU"), |g| g.name.clone());
            capitalize_first(&trf("{dgpu} only (MUX)", &[("dgpu", &dg)])) // translations may start lower case
        }
        "AsusEgpu" => match s.egpu() {
            Some(g) => format!("XG Mobile ({})", g.name),
            None => "XG Mobile".into(),
        },
        other => other.into(),
    }
}

pub fn cw_label(mode: &str, s: &GpuState) -> String {
    let name = s.egpu().or(s.dgpu()).map_or("dGPU", |g| g.name.as_str());
    match mode {
        "integrated" => trf("Integrated – block {name}", &[("name", name)]),
        "hybrid" => tr("Hybrid – all GPUs available"),
        "smart" => trf("Smart – {name} only for approved apps", &[("name", name)]),
        other => capitalize_python(other),
    }
}

/// Python's str.capitalize: first letter upper case, the rest lower case.
fn capitalize_python(s: &str) -> String {
    capitalize_first(&s.to_lowercase())
}

/// runtime_status values from the kernel, as shown in the menu.
fn power_label(power: &str) -> Option<String> {
    matches!(power, "active" | "suspended" | "suspending" | "resuming" | "error").then(|| tr(power))
}

pub fn state_label(g: &Gpu) -> String {
    let power = power_label(&g.power).unwrap_or_else(|| g.power.clone());
    if g.blocked {
        if power.is_empty() {
            tr("blocked")
        } else {
            trf("{power}, blocked", &[("power", &power)])
        }
    } else if power.is_empty() {
        tr("no runtime PM")
    } else {
        power
    }
}

pub fn describe(s: &GpuState) -> String {
    let Some(g) = working_gpu(s) else { return tr("No graphics card detected") };
    let mut text = format!("{} ({})", g.name, g.kind.label());
    // Python compares identity: equal Gpu values are the same entry in practice (unique addresses).
    let others: Vec<String> =
        s.gpus.iter().filter(|x| *x != g).map(|x| format!("{} {}", x.name, state_label(x))).collect();
    if !others.is_empty() {
        text += " · ";
        text += &others.join(", ");
    }
    text
}

pub fn gpu_line(g: &Gpu) -> String {
    let driver = if g.driver.is_empty() { tr("no driver") } else { g.driver.clone() };
    format!("{}: {} – {}, {}", g.kind.label(), g.name, driver, state_label(g))
}

pub fn xg_line(s: &GpuState) -> String {
    if xg_gone(s) {
        tr("XG Mobile: disconnected while in use – reboot needed")
    } else if xg_unlocked(s) {
        tr("XG Mobile: unlocked, still in use – do not disconnect")
    } else if s.egpu_connected {
        tr("XG Mobile: connected")
    } else {
        tr("XG Mobile: not connected")
    }
}

/// Python's repr of a list of strings: ['a', 'b'].
fn py_list<S: AsRef<str>>(items: &[S]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("'{}'", s.as_ref())).collect();
    format!("[{}]", inner.join(", "))
}

/// What `asus-gpu-tray dump` prints; the text of `asus_gpu_tray.py --dump` without the supergfxd lines.
pub fn dump(s: &GpuState, paths: &Paths) -> String {
    let mut out = String::new();
    for g in &s.gpus {
        let d3cold = read(&paths.pci.join(&g.addr).join("d3cold_allowed"));
        let extra = if g.kind != Kind::Igpu && !d3cold.is_empty() {
            format!(", D3cold {}", if d3cold == "1" { "allowed" } else { "disabled" })
        } else {
            String::new()
        };
        let _ = writeln!(out, "{}  {}{}  [{}]", g.addr, gpu_line(g), extra, g.vendor);
    }
    if s.cardwire {
        let _ = writeln!(out, "cardwire: mode {}; available {}", s.cw_mode, py_list(&s.cw_modes));
    } else {
        out += "cardwire: no\n";
    }
    if s.asus_egpu {
        let pending = if s.hw_pending.is_empty() { String::new() } else { format!("; pending {}", s.hw_pending) };
        let _ = writeln!(out, "{}; hardware mode {}{}", xg_line(s), s.hw_mode, pending);
        let backends: Vec<&str> =
            [("live (asus-gpu-live@)", s.live_backend), ("reboot (asus-gpu-switch@)", s.reboot_backend)]
                .into_iter()
                .filter_map(|(name, ok)| ok.then_some(name))
                .collect();
        let backends = if backends.is_empty() { "not installed".to_string() } else { backends.join(", ") };
        let _ = writeln!(out, "hardware switch backends: {backends}");
    }
    let _ = writeln!(out, "rendering: {}", describe(s));
    for m in &s.cw_modes {
        let _ = writeln!(out, "  live mode {m}: {}", cw_label(m, s));
    }
    for m in hw_modes(s) {
        let _ = writeln!(out, "  hardware mode {m}: {}", hw_label(m, s));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::gpu::cardwire::CwDevice;
    use crate::gpu::state::{read_state, CwRepair};
    use crate::gpu::testutil::{write, FakeCmd};

    /// The X16 (GV601RE) in Hybrid mode, XG Mobile not connected, as of 2026-10-08.
    fn x16_tree(root: &std::path::Path) -> Paths {
        let p = Paths::under(root);
        let gpu = |addr: &str, vendor: &str, device: &str, power: &str, driver: &str| {
            let dev = p.pci.join(addr);
            write(&dev.join("class"), "0x030000\n");
            write(&dev.join("vendor"), &format!("0x{vendor}\n"));
            write(&dev.join("device"), &format!("0x{device}\n"));
            write(&dev.join("power/runtime_status"), &format!("{power}\n"));
            fs::create_dir_all(root.join("drivers").join(driver)).unwrap();
            symlink(root.join("drivers").join(driver), dev.join("driver")).unwrap();
        };
        gpu("0000:01:00.0", "10de", "2523", "suspended", "nvidia");
        write(&p.pci.join("0000:01:00.0/d3cold_allowed"), "1\n");
        gpu("0000:3a:00.0", "1002", "1681", "active", "amdgpu");
        write(&p.pci.join("0000:3a:00.3/class"), "0x0c0330\n"); // the APU's USB controller
        for (name, value) in
            [("egpu_connected", "0"), ("egpu_enable", "0"), ("gpu_mux_mode", "1"), ("dgpu_disable", "0")]
        {
            write(&p.attr.join(name).join("current_value"), &format!("{value}\n"));
        }
        write(
            &p.pci_ids,
            "10de  NVIDIA Corporation\n\t2523  GA106M [GeForce RTX 3050 Ti Mobile / Max-Q]\n\
             1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t1681  Rembrandt [Radeon 680M]\n",
        );
        write(&p.unit_dirs[0].join("asus-gpu-live@.service"), "");
        write(&p.unit_dirs[0].join("asus-gpu-switch@.service"), "");
        p
    }

    #[test]
    fn dump_matches_python_on_the_x16() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = x16_tree(tmp.path());
        let gpu = |name: &str, vendor: &str, driver: &str, discrete| CwDevice {
            name: Some(name.into()),
            vendor: Some(vendor.into()),
            driver: driver.into(),
            blocked: false,
            discrete,
        };
        let devices = [
            ("0000:3a:00.0".to_string(), gpu("AMD Radeon 680M", "AMD", "amdgpu", false)),
            ("0000:01:00.0".to_string(), gpu("NVIDIA GeForce RTX 3050 Ti Laptop GPU", "Nvidia", "nvidia", true)),
        ];
        let cmd = FakeCmd::new(&[]).cardwire_devices(devices.into(), "hybrid", &["integrated", "hybrid", "smart"]);
        let s = read_state(&paths, &cmd, &cmd, &mut CwRepair::default());
        let expected = include_str!("../../tests/reference/x16-hybrid-dump.txt");
        assert_eq!(dump(&s, &paths), expected);
        assert!(!cmd.calls().iter().any(|c| c.starts_with("nvidia-smi")));
    }
}

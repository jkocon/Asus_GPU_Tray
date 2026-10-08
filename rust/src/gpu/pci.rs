//! GPU detection from sysfs. Only kernel-cached sysfs files are read - lspci reads config space
//! and wakes a suspended dGPU.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use super::cardwire::{self, CwDevices};
use super::paths::{file_name, read, sorted_entries, Paths};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Igpu,
    Dgpu,
    Egpu,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Igpu => "iGPU",
            Kind::Dgpu => "dGPU",
            Kind::Egpu => "eGPU",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpu {
    pub addr: String,
    /// "NVIDIA" / "AMD" / "Intel" / raw vendor ID
    pub vendor: String,
    pub name: String,
    pub driver: String,
    pub kind: Kind,
    /// runtime_status from sysfs
    pub power: String,
    /// hidden from new apps by cardwire
    pub blocked: bool,
}

pub fn vendor_name(id: &str) -> String {
    match id {
        "10de" => "NVIDIA",
        "1002" => "AMD",
        "8086" => "Intel",
        other => other,
    }
    .to_string()
}

/// (pci.ids file, vendor, device) -> name
type NameCache = Mutex<HashMap<(String, String, String), String>>;

/// Device name from pci.ids, cached per (file, vendor, device).
pub fn pci_ids_name(file: &Path, vendor: &str, device: &str) -> String {
    static CACHE: OnceLock<NameCache> = OnceLock::new();
    let key = (file.to_string_lossy().into_owned(), vendor.to_string(), device.to_string());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(name) = cache.lock().unwrap().get(&key) {
        return name.clone();
    }
    let name = fs::read(file).map(|b| lookup_pci_ids(&String::from_utf8_lossy(&b), vendor, device)).unwrap_or_default();
    cache.lock().unwrap().insert(key, name.clone());
    name
}

fn lookup_pci_ids(text: &str, vendor: &str, device: &str) -> String {
    let mut in_vendor = false;
    let dev_prefix = format!("\t{device}");
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue; // comments also appear inside a vendor block
        }
        if !line.starts_with('\t') {
            in_vendor = line.starts_with(vendor);
        } else if in_vendor && line.starts_with(&dev_prefix) {
            let line = line.trim_start();
            return line.split_once(char::is_whitespace).map(|(_, rest)| rest.trim()).unwrap_or("").to_string();
        }
    }
    String::new()
}

/// "GA104M [GeForce RTX 3070 Mobile / Max-Q]" -> "RTX 3070"
pub fn short_name(pci_ids: &Path, vendor: &str, device: &str) -> String {
    short_name_from(&pci_ids_name(pci_ids, vendor, device), vendor, device)
}

pub fn short_name_from(pci_ids_name: &str, vendor: &str, device: &str) -> String {
    let mut name = if pci_ids_name.is_empty() { format!("{vendor}:{device}") } else { pci_ids_name.to_string() };
    if let (Some(open), Some(close)) = (name.find('['), name.rfind(']')) {
        let inner = if open < close { &name[open + 1..close] } else { "" };
        if !inner.is_empty() {
            name = inner.to_string();
        }
    }
    let name = name.split(" / ").next().unwrap_or("");
    name.replace("GeForce ", "").replace(" Mobile", "").replace(" Max-Q", "").trim().to_string()
}

pub fn is_integrated(pci: &Path, dev: &Path, vendor: &str) -> bool {
    let name = file_name(dev);
    let mut parts = name.splitn(3, ':');
    let (domain, bus) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    match vendor {
        "8086" => bus == "00", // Intel iGPU lives on bus 0, Arc cards sit behind a PCIe bridge
        // AMD APU: the CPU's USB controllers share the GPU's bus; a dGPU only has HDMI audio next to it
        "1002" => {
            let prefix = format!("{domain}:{bus}:");
            sorted_entries(pci)
                .iter()
                .any(|sib| file_name(sib).starts_with(&prefix) && read(&sib.join("class")).starts_with("0x0c03"))
        }
        _ => false,
    }
}

/// Thunderbolt/USB4: the kernel marks ports that lead outside the machine.
pub fn is_external(dev: &Path) -> bool {
    if read(&dev.join("removable")) == "removable" {
        return true;
    }
    let Ok(real) = fs::canonicalize(dev) else { return false };
    real.ancestors()
        .skip(1)
        .filter(|p| file_name(p).matches(':').count() == 2)
        .any(|p| read(&p.join("external_facing")) == "1")
}

/// Strips the "0x" of sysfs IDs.
fn id(text: &str) -> &str {
    text.get(2..).unwrap_or("")
}

pub fn detect_gpus(paths: &Paths, cw: &CwDevices) -> Vec<Gpu> {
    let xg_active = paths.read_attr("egpu_enable") == "1";
    let mut gpus = Vec::new();
    let mut seen = Vec::new();
    for dev in sorted_entries(&paths.pci) {
        if !read(&dev.join("class")).starts_with("0x03") {
            continue;
        }
        let (vendor_raw, device_raw) = (read(&dev.join("vendor")), read(&dev.join("device")));
        let (vendor, device) = (id(&vendor_raw), id(&device_raw));
        let kind = if is_integrated(&paths.pci, &dev, vendor) {
            Kind::Igpu
        } else if is_external(&dev) || (xg_active && vendor == "10de") {
            Kind::Egpu // XG Mobile takes over the same port as the built-in dGPU
        } else {
            Kind::Dgpu
        };
        let driver = dev.join("driver");
        let addr = file_name(&dev);
        gpus.push(Gpu {
            vendor: vendor_name(vendor),
            name: short_name(&paths.pci_ids, vendor, device),
            driver: fs::canonicalize(&driver).map(|p| file_name(&p)).unwrap_or_default(),
            kind,
            power: read(&dev.join("power/runtime_status")),
            blocked: cw.get(&addr).is_some_and(|d| d.blocked),
            addr: addr.clone(),
        });
        seen.push(addr);
    }
    // cardwire hides a blocked GPU's sysfs files from everyone, so take it from cardwire itself.
    for (addr, d) in cw {
        if seen.contains(addr) || !d.blocked {
            continue;
        }
        let dev = paths.pci.join(addr);
        if fs::symlink_metadata(&dev).is_err() {
            continue; // removed from the bus (XG Mobile unplugged); cardwire still lists it
        }
        let vendor = match d.vendor.as_deref() {
            Some("Nvidia") => "NVIDIA".to_string(),
            Some(v) => v.to_string(),
            None => "?".to_string(),
        };
        let kind = if !d.discrete {
            Kind::Igpu
        } else if is_external(&dev) || (xg_active && vendor == "NVIDIA") {
            Kind::Egpu
        } else {
            Kind::Dgpu
        };
        gpus.push(Gpu {
            addr: addr.clone(),
            vendor,
            name: cardwire::short_name(d.name.as_deref().unwrap_or(addr)),
            driver: d.driver.clone(),
            kind,
            power: String::new(),
            blocked: true,
        });
    }
    gpus.sort_by(|a, b| a.addr.cmp(&b.addr));
    gpus
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::cardwire::CwDevice;
    use crate::gpu::testutil::write;

    #[test]
    fn short_name_survives_odd_pci_ids_entries() {
        assert_eq!(short_name_from("Weird [name without end", "10de", "1234"), "Weird [name without end");
        assert_eq!(short_name_from("GA104M [GeForce RTX 3070 Mobile / Max-Q]", "10de", "249d"), "RTX 3070");
        assert_eq!(short_name_from("", "10de", "1234"), "10de:1234");
        assert_eq!(short_name_from("Odd ]x[ name", "10de", "1"), "Odd ]x[ name");
    }

    #[test]
    fn pci_ids_lookup() {
        let ids = "# comment\n10de  NVIDIA Corporation\n\t2523  GA106M [GeForce RTX 3050 Ti Mobile / Max-Q]\n\
                   \t\t1043 1234  subsystem\n1002  AMD\n\t1681  Rembrandt [Radeon 680M]\n";
        assert_eq!(lookup_pci_ids(ids, "10de", "2523"), "GA106M [GeForce RTX 3050 Ti Mobile / Max-Q]");
        assert_eq!(lookup_pci_ids(ids, "1002", "1681"), "Rembrandt [Radeon 680M]");
        assert_eq!(lookup_pci_ids(ids, "10de", "1681"), "");
    }

    #[test]
    fn removed_gpu_is_not_taken_from_cardwire() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::under(tmp.path());
        fs::create_dir_all(&paths.pci).unwrap();
        write(&paths.attr.join("egpu_enable/current_value"), "1");
        let cw: CwDevices = [(
            "0000:01:00.0".to_string(),
            CwDevice {
                blocked: true,
                discrete: true,
                vendor: Some("Nvidia".into()),
                name: Some("NVIDIA GeForce RTX 3070".into()),
                ..Default::default()
            },
        )]
        .into();
        assert_eq!(detect_gpus(&paths, &cw), vec![]);
        fs::create_dir(paths.pci.join("0000:01:00.0")).unwrap(); // still on the bus, only hidden by cardwire
        let gpus = detect_gpus(&paths, &cw);
        assert_eq!(gpus.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["RTX 3070"]);
        assert_eq!(gpus[0].kind, Kind::Egpu);
    }
}

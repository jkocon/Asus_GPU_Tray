//! cardwire: GPU access modes (Integrated / Hybrid / Smart). Its CLI is the API for now.

use std::collections::BTreeMap;

use serde_json::Value;

use super::cmd::Cmd;
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

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `cardwire list --json` -> devices; None when the output is not what cardwire prints.
pub fn parse_list(json: &str) -> Option<CwDevices> {
    let Value::Object(devices) = serde_json::from_str::<Value>(json).ok()? else { return None };
    let mut out = CwDevices::new();
    for d in devices.values() {
        let pci = d.get("pci")?.as_str()?.to_string();
        let text = |key: &str| d.get(key).and_then(Value::as_str).map(str::to_string);
        out.insert(
            pci,
            CwDevice {
                name: text("name"),
                vendor: text("vendor"),
                driver: text("driver").unwrap_or_default(),
                blocked: truthy(d.get("blocked")),
                discrete: truthy(d.get("discrete")),
            },
        );
    }
    Some(out)
}

/// None when cardwire is not available.
pub fn devices(cmd: &dyn Cmd) -> Option<CwDevices> {
    if !cmd.which("cardwire") {
        return None;
    }
    let out = cmd.run(&["cardwire", "list", "--json"]);
    parse_list(if out.is_empty() { "null" } else { &out })
}

/// "NVIDIA GeForce RTX 3070 Laptop GPU" -> "RTX 3070"
pub fn short_name(name: &str) -> String {
    let mut name = name.to_string();
    for word in ["NVIDIA ", "GeForce ", "AMD ", "Intel(R) ", "Intel ", " Laptop GPU", " Mobile"] {
        name = name.replace(word, "");
    }
    name.trim().to_string()
}

/// "Current Mode: Hybrid\nAvailable Mode: integrated, hybrid, smart" -> ("hybrid", [modes in menu order])
pub fn parse_get(out: &str) -> (String, Vec<String>) {
    let (mut mode, mut modes) = (String::new(), Vec::<String>::new());
    for line in out.lines() {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        match key.trim() {
            "Current Mode" => mode = value.trim().to_lowercase(),
            "Available Mode" => {
                modes = value.split(',').map(|m| m.trim().to_lowercase()).filter(|m| !m.is_empty()).collect()
            }
            _ => {}
        }
    }
    let known = MODES.iter().filter(|m| modes.iter().any(|x| x.as_str() == **m)).map(|m| m.to_string());
    let other = modes.iter().filter(|m| !MODES.contains(&m.as_str())).cloned();
    (mode, known.chain(other).collect())
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
    fn parse_cardwire_get() {
        let (mode, modes) = parse_get("Current Mode: Smart\nAvailable Mode: hybrid, manual, integrated");
        assert_eq!(mode, "smart");
        assert_eq!(modes, ["integrated", "hybrid", "manual"]);
    }

    #[test]
    fn parse_cardwire_list() {
        let json = r#"{"0": {"name": "AMD Radeon 680M", "pci": "0000:3a:00.0", "discrete": false,
                              "vendor": "AMD", "driver": "amdgpu", "blocked": false},
                       "1": {"name": "NVIDIA GeForce RTX 3050 Ti Laptop GPU", "pci": "0000:01:00.0",
                              "discrete": true, "vendor": "Nvidia", "driver": "nvidia", "blocked": true}}"#;
        let cw = parse_list(json).unwrap();
        assert_eq!(cw.len(), 2);
        assert!(cw["0000:01:00.0"].blocked && cw["0000:01:00.0"].discrete);
        assert_eq!(cw["0000:3a:00.0"].driver, "amdgpu");
        assert_eq!(parse_list("null"), None);
        assert_eq!(parse_list(r#"{"0": {"name": "no pci"}}"#), None);
        assert_eq!(short_name("NVIDIA GeForce RTX 3050 Ti Laptop GPU"), "RTX 3050 Ti");
    }
}

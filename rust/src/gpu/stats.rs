//! Live numbers of an NVIDIA GPU that is awake anyway.

use super::cmd::Cmd;
use super::pci::Gpu;

/// nvidia-smi "temperature, power, utilization, memory used, memory total" (csv, no units) ->
/// "54 °C · 38 W · 12 % · 1.2/4.0 GB". Implausible or [N/A] fields are left out (the first power
/// reading after a wake-up can be several hundred watts).
pub fn parse_gpu_stats(out: &str) -> String {
    let Some(first) = out.lines().next().filter(|_| !out.trim().is_empty()) else { return String::new() };
    let fields: Vec<&str> = first.split(',').map(str::trim).collect();
    if fields.len() != 5 {
        return String::new();
    }
    let num = |text: &str, limit: f64| text.parse::<f64>().ok().filter(|v| (0.0..=limit).contains(v));
    let (temp, power, util) = (num(fields[0], 150.0), num(fields[1], 400.0), num(fields[2], 100.0));
    let (used, total) = (num(fields[3], 1e6), num(fields[4], 1e6));
    let mut parts = Vec::new();
    if let Some(t) = temp {
        parts.push(format!("{t:.0} °C"));
    }
    if let Some(p) = power {
        parts.push(format!("{p:.0} W"));
    }
    if let Some(u) = util {
        parts.push(format!("{u:.0} %"));
    }
    if let (Some(u), Some(t)) = (used, total.filter(|t| *t != 0.0)) {
        parts.push(format!("{:.1}/{:.1} GB", u / 1024.0, t / 1024.0));
    }
    parts.join(" · ")
}

/// Never for a suspended or blocked card: nvidia-smi opens the device and would wake it. Only
/// called when the user opens the menu or clicks the icon, never from the polling timer, so it
/// does not keep the card awake either.
pub fn gpu_stats(g: &Gpu, cmd: &dyn Cmd) -> String {
    if g.vendor != "NVIDIA" || g.blocked || g.power != "active" || g.driver != "nvidia" {
        return String::new();
    }
    if !cmd.which("nvidia-smi") {
        return String::new();
    }
    parse_gpu_stats(&cmd.run(&[
        "nvidia-smi",
        &format!("--id={}", g.addr),
        "--format=csv,noheader,nounits",
        "--query-gpu=temperature.gpu,power.draw,utilization.gpu,memory.used,memory.total",
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::testutil::{dgpu, igpu, FakeCmd};

    #[test]
    fn parse() {
        assert_eq!(parse_gpu_stats("54, 38.12, 12, 1234, 4096"), "54 °C · 38 W · 12 % · 1.2/4.0 GB");
        assert_eq!(parse_gpu_stats("42, 752.67, 2, 2, 4096"), "42 °C · 2 % · 0.0/4.0 GB"); // bogus power
        assert_eq!(parse_gpu_stats("[N/A], [N/A], 0, 5, 8192"), "0 % · 0.0/8.0 GB");
        assert_eq!(parse_gpu_stats(""), "");
    }

    #[test]
    fn never_wakes_a_sleeping_card() {
        let cmd = FakeCmd::new(&["nvidia-smi"]);
        assert_eq!(gpu_stats(&dgpu(), &cmd), ""); // suspended
        let blocked = Gpu { power: "active".into(), blocked: true, ..dgpu() };
        assert_eq!(gpu_stats(&blocked, &cmd), "");
        assert_eq!(gpu_stats(&igpu(), &cmd), "");
        assert!(cmd.calls().is_empty());
        let awake = Gpu { power: "active".into(), ..dgpu() };
        let cmd = cmd.out(
            "nvidia-smi --id=0000:01:00.0 --format=csv,noheader,nounits \
             --query-gpu=temperature.gpu,power.draw,utilization.gpu,memory.used,memory.total",
            "54, 38.12, 12, 1234, 4096",
        );
        assert_eq!(gpu_stats(&awake, &cmd), "54 °C · 38 W · 12 % · 1.2/4.0 GB");
        assert_eq!(cmd.calls().len(), 1);
    }
}

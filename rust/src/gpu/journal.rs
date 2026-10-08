//! Kernel log: NVIDIA cards that fell off the bus.

use std::collections::HashMap;
use std::fs;
use std::time::UNIX_EPOCH;

use super::paths::{read, Paths};
use super::pci::Gpu;
use super::state::GpuState;

/// What journalctl -k is filtered with (grep -E syntax). "non-zero usage count": the GPU went away
/// while the driver still had users (XG Mobile unlocked or its power unplugged while active) - the
/// driver is wedged, also when the card shows up again.
pub const GPU_LOST_RE: &str =
    "fallen off the bus|Unable to change power state from D3cold to D0|with non-zero usage count";

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// First `\b[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\b` in the text.
fn find_slot(text: &str) -> Option<&str> {
    let b = text.as_bytes();
    let shape = [4, 2, 2];
    (0..b.len()).find_map(|start| {
        if start > 0 && is_word(b[start - 1]) {
            return None;
        }
        let mut i = start;
        for (n, len) in shape.iter().enumerate() {
            if n > 0 {
                if b.get(i) != Some(&b':') {
                    return None;
                }
                i += 1;
            }
            if i + len > b.len() || !b[i..i + len].iter().all(|&c| is_hex(c)) {
                return None;
            }
            i += len;
        }
        if i < b.len() && is_word(b[i]) {
            return None;
        }
        Some(&text[start..i])
    })
}

/// A `journalctl -o short-unix` line matching GPU_LOST_RE -> (PCI slot "0000:01:00", time).
pub fn parse_gpu_lost(line: &str) -> Option<(String, f64)> {
    let (stamp, text) = line.split_once(' ').unwrap_or((line, ""));
    let slot = find_slot(text)?;
    let time = stamp.trim().parse::<f64>().ok()?;
    Some((slot.to_string(), time))
}

/// When the last live switch succeeded: it re-creates the NVIDIA devices, so older errors are gone.
pub fn last_live_switch(paths: &Paths) -> f64 {
    if !read(&paths.live_result).starts_with("Switched") {
        return 0.0;
    }
    fs::metadata(&paths.live_result)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64())
}

/// NVIDIA cards that fell off the bus: a kernel message newer than the card, or runtime PM in error.
pub fn lost_gpus<'a>(s: &'a GpuState, events: &HashMap<String, f64>, since: f64) -> Vec<&'a Gpu> {
    s.gpus
        .iter()
        .filter(|g| {
            let slot = g.addr.rsplit_once('.').map_or(g.addr.as_str(), |(slot, _)| slot);
            g.vendor == "NVIDIA" && (g.power == "error" || events.get(slot).copied().unwrap_or(0.0) > since)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::testutil::{egpu, igpu, state};

    const XID: &str =
        "1759600000.5 host kernel: NVRM: Xid (PCI:0000:01:00): 79, pid='<unknown>', GPU has fallen off the bus.";
    const D3: &str = "1759600001.0 host kernel: nvidia 0000:01:00.0: Unable to change power state from D3cold to D0, device inaccessible";
    const USAGE: &str =
        "1759600002.0 host kernel: NVRM: Attempting to remove device 0000:01:00.0 with non-zero usage count!";

    #[test]
    fn pattern_matches_every_kind_of_loss() {
        for line in [XID, D3, USAGE] {
            assert!(GPU_LOST_RE.split('|').any(|alt| line.contains(alt)), "{line}");
        }
    }

    #[test]
    fn parse() {
        assert_eq!(parse_gpu_lost(XID), Some(("0000:01:00".into(), 1759600000.5)));
        assert_eq!(parse_gpu_lost(D3), Some(("0000:01:00".into(), 1759600001.0)));
        assert_eq!(parse_gpu_lost(USAGE), Some(("0000:01:00".into(), 1759600002.0)));
        assert_eq!(parse_gpu_lost("-- No entries --"), None);
        assert_eq!(parse_gpu_lost("1 x 10000:01:00 y"), None); // no word boundary
    }

    #[test]
    fn only_errors_newer_than_the_last_switch() {
        let events = HashMap::from([("0000:01:00".to_string(), 100.0)]);
        let s = state();
        assert_eq!(lost_gpus(&s, &events, 50.0), vec![&egpu()]);
        assert!(lost_gpus(&s, &events, 150.0).is_empty());
        let broken = Gpu { power: "error".into(), ..egpu() };
        let s = GpuState { gpus: vec![broken.clone(), igpu()], ..state() };
        assert_eq!(lost_gpus(&s, &HashMap::new(), 0.0), vec![&broken]);
    }
}

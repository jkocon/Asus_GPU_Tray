//! The settings file QSettings("asus-gpu-tray", "asus-gpu-tray") writes, shared with the Python tray:
//! ~/.config/asus-gpu-tray/asus-gpu-tray.conf, key notify_dgpu_wake in [General].

use std::fs;
use std::path::PathBuf;

const KEY: &str = "notify_dgpu_wake";

pub struct Settings {
    path: PathBuf,
}

impl Settings {
    pub fn new() -> Self {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        Settings { path: config.join("asus-gpu-tray/asus-gpu-tray.conf") }
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Settings { path }
    }

    fn general_value(text: &str) -> Option<String> {
        let mut in_general = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_general = line == "[General]";
            } else if in_general {
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == KEY {
                        return Some(v.trim().to_string());
                    }
                }
            }
        }
        None
    }

    /// Default on, as in the Python tray.
    pub fn notify_wake(&self) -> bool {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        Self::general_value(&text).is_none_or(|v| v != "false")
    }

    pub fn set_notify_wake(&self, on: bool) {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        let entry = format!("{KEY}={on}");
        let mut out = Vec::new();
        let (mut in_general, mut seen_general, mut done) = (false, false, false);
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                if in_general && !done {
                    out.push(entry.clone());
                    done = true;
                }
                in_general = t == "[General]";
                seen_general |= in_general;
            } else if in_general && t.split_once('=').is_some_and(|(k, _)| k.trim() == KEY) {
                if !done {
                    out.push(entry.clone());
                    done = true;
                }
                continue;
            }
            out.push(line.to_string());
        }
        if !done {
            if !seen_general {
                out.insert(0, "[General]".into());
                out.insert(1, entry);
            } else {
                out.push(entry);
            }
        }
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&self.path, out.join("\n") + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_and_write_keep_other_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/asus-gpu-tray.conf");
        let s = Settings::at(path.clone());
        assert!(s.notify_wake());
        s.set_notify_wake(false);
        assert!(!s.notify_wake());
        assert_eq!(fs::read_to_string(&path).unwrap(), "[General]\nnotify_dgpu_wake=false\n");
        fs::write(&path, "[Other]\nx=1\n[General]\nnotify_dgpu_wake=false\ny=2\n").unwrap();
        s.set_notify_wake(true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[Other]\nx=1\n[General]\nnotify_dgpu_wake=true\ny=2\n");
        assert!(s.notify_wake());
    }
}

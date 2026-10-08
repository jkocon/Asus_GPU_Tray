//! Processes that hold the NVIDIA card. Reading /proc does not wake the card.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::paths::{file_name, read, sorted_entries, Paths};
use super::pci::Gpu;

/// Apps the tray may restart after freeing the card, with the arguments they need for that.
pub const RESTARTABLE_APPS: &[(&str, &[&str])] = &[("rog-control-center", &["--background"])];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    /// start time from /proc/<pid>/stat, so a reused PID is never mistaken for it
    pub start: String,
    pub exe: String,
    pub args: Vec<String>,
}

impl Proc {
    pub fn name(&self) -> String {
        file_name(Path::new(&self.exe))
    }

    pub fn alive(&self) -> bool {
        proc_start(self.pid) == self.start
    }

    pub fn restart_cmd(&self) -> Vec<String> {
        let name = self.name();
        let extra = RESTARTABLE_APPS.iter().find(|(n, _)| *n == name).map_or(&[][..], |(_, a)| *a);
        let mut cmd = vec![self.exe.clone()];
        cmd.extend(self.args.iter().skip(1).cloned());
        cmd.extend(extra.iter().filter(|a| !self.args.iter().any(|x| x.as_str() == **a)).map(|a| a.to_string()));
        cmd
    }
}

pub fn proc_start(pid: i32) -> String {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { return String::new() };
    let after_comm = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
    after_comm.split_whitespace().nth(19).unwrap_or("").to_string() // field 22: starttime
}

fn getuid() -> u32 {
    // SAFETY: getuid cannot fail and has no side effects.
    unsafe { libc::getuid() }
}

pub fn is_root() -> bool {
    // SAFETY: geteuid cannot fail and has no side effects.
    unsafe { libc::geteuid() == 0 }
}

/// This user's processes (everyone's with all_users, as root), except this program itself
/// (kernel threads have no executable).
pub fn own_processes(all_users: bool) -> Vec<Proc> {
    let me = std::process::id() as i32;
    let uid = getuid();
    let mut out = Vec::new();
    for proc in sorted_entries(Path::new("/proc")) {
        let Ok(pid) = file_name(&proc).parse::<i32>() else { continue };
        if pid == me {
            continue;
        }
        let entry = || -> Option<Proc> {
            if !all_users && fs::metadata(&proc).ok()?.uid() != uid {
                return None;
            }
            let exe = fs::read_link(proc.join("exe")).ok()?.to_string_lossy().into_owned();
            let exe = exe.strip_suffix(" (deleted)").map(str::to_string).unwrap_or(exe);
            let cmdline = fs::read(proc.join("cmdline")).ok()?;
            let args =
                String::from_utf8_lossy(&cmdline).split('\0').filter(|a| !a.is_empty()).map(str::to_string).collect();
            Some(Proc { pid, start: proc_start(pid), exe, args })
        };
        out.extend(entry());
    }
    out
}

/// This user's processes whose executable (not argv[0]) has one of the given names.
pub fn user_processes(names: &[&str]) -> Vec<Proc> {
    own_processes(false).into_iter().filter(|p| names.contains(&p.name().as_str())).collect()
}

fn char_devices(dir: &Path, matches: impl Fn(&str) -> bool) -> impl Iterator<Item = PathBuf> {
    sorted_entries(dir)
        .into_iter()
        .filter(move |p| matches(&file_name(p)) && fs::metadata(p).is_ok_and(|m| m.file_type().is_char_device()))
}

fn drm_nodes(paths: &Paths, addr: &str) -> impl Iterator<Item = String> {
    let dri = paths.dev.join("dri");
    sorted_entries(&paths.pci.join(addr).join("drm"))
        .into_iter()
        .map(|n| file_name(&n))
        .filter(|n| n.starts_with("card") || n.starts_with("renderD"))
        .map(move |n| dri.join(n).to_string_lossy().into_owned())
}

/// /dev/nvidia* and the DRM nodes of the NVIDIA cards - what the live switch needs free.
pub fn nvidia_nodes(paths: &Paths) -> HashSet<String> {
    let mut nodes: HashSet<String> =
        char_devices(&paths.dev, |n| n.starts_with("nvidia")).map(|p| p.to_string_lossy().into_owned()).collect();
    nodes.extend(sorted_entries(&paths.dev.join("nvidia-caps")).iter().map(|p| p.to_string_lossy().into_owned()));
    for dev in sorted_entries(&paths.pci) {
        if read(&dev.join("vendor")) == "0x10de" {
            nodes.extend(drm_nodes(paths, &file_name(&dev)));
        }
    }
    nodes
}

/// This user's processes that have this GPU itself open (/dev/nvidiaN or its DRM nodes) - not
/// the control nodes (/dev/nvidiactl, -uvm, -modeset), which apps open just to list GPUs.
pub fn gpu_users(paths: &Paths, g: &Gpu) -> Vec<Proc> {
    let numbered = |n: &str| n.strip_prefix("nvidia").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
    let mut nodes: HashSet<String> =
        char_devices(&paths.dev, numbered).map(|p| p.to_string_lossy().into_owned()).collect();
    nodes.extend(drm_nodes(paths, &g.addr));
    card_holders(&nodes, false)
}

pub fn holds(pid: i32, nodes: &HashSet<String>) -> bool {
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let Ok(fds) = fs::read_dir(&fd_dir) else { return false };
    fds.flatten().any(|fd| {
        // closed in the meantime -> Err
        fs::read_link(fd.path()).is_ok_and(|target| nodes.contains(&*target.to_string_lossy()))
    })
}

/// Processes that have one of the nodes open.
pub fn card_holders(nodes: &HashSet<String>, all_users: bool) -> Vec<Proc> {
    if nodes.is_empty() {
        return Vec::new();
    }
    own_processes(all_users).into_iter().filter(|p| holds(p.pid, nodes)).collect()
}

/// One line per program: "• firefox (PID 1234, 1240)".
pub fn describe_procs(procs: &[Proc]) -> String {
    let mut pids: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for p in procs {
        pids.entry(p.name()).or_default().push(p.pid);
    }
    pids.iter()
        .map(|(name, ids)| {
            let shown: Vec<String> = ids.iter().take(4).map(i32::to_string).collect();
            let more = if ids.len() > 4 { format!(", … {} processes", ids.len()) } else { String::new() };
            format!("• {name} (PID {}{more})", shown.join(", "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn stop_processes(procs: &[Proc], timeout: Duration) {
    let signal_alive = |sig: i32| {
        for p in procs.iter().filter(|p| p.alive()) {
            // SAFETY: plain syscall; a vanished or foreign process only makes it return an error.
            unsafe { libc::kill(p.pid, sig) };
        }
    };
    signal_alive(libc::SIGTERM);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && procs.iter().any(Proc::alive) {
        thread::sleep(Duration::from_millis(100));
    }
    signal_alive(libc::SIGKILL);
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::process::{Child, Command, Stdio};

    use super::*;

    struct Sleeper(Child);

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn(stdin: Stdio) -> Sleeper {
        let child = Command::new("sleep").arg("30").stdin(stdin).spawn().unwrap();
        thread::sleep(Duration::from_millis(200));
        Sleeper(child)
    }

    #[test]
    fn stop_and_restart_command() {
        let mut p = spawn(Stdio::null());
        let pid = p.0.id() as i32;
        let found: Vec<Proc> = user_processes(&["sleep"]).into_iter().filter(|x| x.pid == pid).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name(), "sleep");
        assert_eq!(found[0].restart_cmd()[1..], ["30"]);
        stop_processes(&found, Duration::from_secs(2));
        assert!(p.0.wait().is_ok());
    }

    #[test]
    fn reused_pid_is_left_alone() {
        let mut p = spawn(Stdio::null());
        let stale = Proc { pid: p.0.id() as i32, start: "0".into(), exe: "/usr/bin/sleep".into(), args: vec![] };
        stop_processes(&[stale], Duration::from_millis(300));
        assert!(p.0.try_wait().unwrap().is_none());
    }

    #[test]
    fn restart_uses_the_real_executable() {
        let p = Proc {
            pid: 1,
            start: "1".into(),
            exe: "/usr/bin/rog-control-center".into(),
            args: vec!["rog-control-center".into(), "--autostart".into()],
        };
        assert_eq!(p.restart_cmd(), ["/usr/bin/rog-control-center", "--autostart", "--background"]);
    }

    #[test]
    fn card_holders_finds_open_node() {
        let node = tempfile::NamedTempFile::new().unwrap();
        let p = spawn(Stdio::from(File::open(node.path()).unwrap()));
        let nodes = HashSet::from([node.path().to_string_lossy().into_owned()]);
        let found: Vec<i32> = card_holders(&nodes, false).iter().map(|x| x.pid).collect();
        assert!(found.contains(&(p.0.id() as i32)));
        assert!(!found.contains(&(std::process::id() as i32)));
        assert!(card_holders(&HashSet::new(), false).is_empty());
    }

    #[test]
    fn describe_groups_by_program() {
        let procs: Vec<Proc> = (1..7)
            .map(|i| Proc { pid: i, start: "1".into(), exe: "/usr/lib/firefox/firefox".into(), args: vec![] })
            .collect();
        assert_eq!(describe_procs(&procs), "• firefox (PID 1, 2, 3, 4, … 6 processes)");
    }
}

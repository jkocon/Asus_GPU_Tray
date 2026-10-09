//! The root helpers behind the systemd units and the udev rule (phase 3 of the rewrite). Each one
//! is a faithful port of a script in `scripts/`; that script stays the reference.

pub mod egpu_power;
pub mod pcie;
pub mod switch_apply;
pub mod switch_live;
pub mod switch_reboot;
#[cfg(test)]
pub mod testutil;

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::gpu::cmd::run_with_timeout;
use crate::gpu::paths::Paths;

/// Kernel messages after which the NVIDIA driver is wedged (as in the scripts).
pub const KERNEL_LOSS_RE: &str = "(NVRM|nvidia).*(fallen off the bus|with non-zero usage count|D3cold to D0)";

/// A line in the system journal under the tray's tag, as `logger -t asus-gpu-tray` in the scripts.
pub fn log(msg: &str) {
    let _ = Command::new("logger").args(["-t", "asus-gpu-tray", "--", msg]).status();
}

/// The shared switch lock (flock, like the scripts' `exec 9> lock; flock -n 9`). None when another
/// switch holds it.
pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == io::ErrorKind::WouldBlock { Ok(None) } else { Err(err) };
    }
    Ok(Some(file))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// TERM, INT and HUP only set a flag (RealOps::interrupted), so a helper can put the hardware back
/// before it exits, like the scripts' traps.
pub fn catch_signals() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
}

/// What the helpers do to the system beyond sysfs files, behind a trait for the tests.
pub trait Ops {
    /// A line for the unit's journal (the scripts' `echo`).
    fn say(&self, msg: &str);
    /// This boot's kernel log shows a lost GPU (KERNEL_LOSS_RE).
    fn kernel_lost_gpu(&self) -> bool;
    fn sync(&self);
    fn sleep(&self, d: Duration);
    fn sysrq(&self, paths: &Paths, key: char);
    /// `systemctl reboot`; its exit code.
    fn reboot(&self) -> u8;
    /// Runs a command (setpci, systemctl, udevadm, ...): its output, or None when it failed or is
    /// missing.
    fn run(&self, args: &[&str]) -> Option<String>;
    /// Runs a command and hands each line of its output to `on_line` as it comes; its exit code
    /// (-1 when it could not run or was killed).
    fn stream(&self, args: &[&str], on_line: &mut dyn FnMut(&str)) -> i32;
    /// A TERM, INT or HUP arrived (catch_signals).
    fn interrupted(&self) -> bool;
}

/// Sets the mode in supergfxd's config, when supergfxd is installed (as the scripts' python3).
pub fn set_supergfxd_mode(paths: &Paths, mode: &str) -> io::Result<bool> {
    let path = &paths.supergfxd_conf;
    if !path.exists() {
        return Ok(false);
    }
    let mut cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let obj = cfg.as_object_mut().ok_or_else(|| io::Error::other("supergfxd.conf is not a JSON object"))?;
    obj.insert("mode".into(), mode.into());
    obj.insert("pending_mode".into(), serde_json::Value::Null);
    obj.insert("pending_action".into(), serde_json::Value::Null);
    std::fs::write(path, serde_json::to_string_pretty(&cfg)?)?;
    Ok(true)
}

pub struct RealOps;

impl Ops for RealOps {
    fn say(&self, msg: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    fn kernel_lost_gpu(&self) -> bool {
        let args = ["journalctl", "-k", "-b", "-q", "--no-pager", "--grep", KERNEL_LOSS_RE];
        !run_with_timeout(&args, Duration::from_secs(60)).is_empty()
    }

    fn sync(&self) {
        // SAFETY: sync(2) takes no arguments and cannot fail.
        unsafe { libc::sync() };
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    fn sysrq(&self, paths: &Paths, key: char) {
        let _ = std::fs::write(&paths.sysrq, key.to_string());
    }

    fn reboot(&self) -> u8 {
        let status = Command::new("systemctl").arg("reboot").stdin(Stdio::null()).status();
        status.ok().and_then(|s| s.code()).map_or(1, |c| c.clamp(0, 255) as u8)
    }

    fn run(&self, args: &[&str]) -> Option<String> {
        let (prog, rest) = args.split_first()?;
        let out = Command::new(prog).args(rest).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn stream(&self, args: &[&str], on_line: &mut dyn FnMut(&str)) -> i32 {
        let Some((prog, rest)) = args.split_first() else { return -1 };
        let Ok(mut child) = Command::new(prog).args(rest).stdin(Stdio::null()).stdout(Stdio::piped()).spawn() else {
            return -1;
        };
        let stdout = child.stdout.take().expect("stdout is piped");
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => on_line(&line),
                Err(_) => break,
            }
        }
        child.wait().ok().and_then(|s| s.code()).unwrap_or(-1)
    }

    fn interrupted(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }
}

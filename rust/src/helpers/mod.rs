//! The root helpers behind the systemd units and the udev rule (phase 3 of the rewrite). Each one
//! is a faithful port of a script in `scripts/`; that script stays the reference.

pub mod egpu_power;
pub mod switch_reboot;
#[cfg(test)]
pub mod testutil;

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
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
}

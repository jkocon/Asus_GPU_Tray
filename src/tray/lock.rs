//! One tray per session. The same lock file as asus_gpu_tray.py, so the two exclude each other.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

fn lock_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir()) {
        return Ok(dir);
    }
    // Private per-user directory; never a predictable path in /tmp that someone else could plant.
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| io::Error::other("HOME is not set"))?;
    let dir = home.join(".cache/asus-gpu-tray");
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    Ok(dir)
}

/// The locked file (keep it open), or None when the tray already runs in this session.
pub fn single_instance_lock() -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(lock_dir()?.join("asus-gpu-tray.lock"))?;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == io::ErrorKind::WouldBlock { Ok(None) } else { Err(err) };
    }
    Ok(Some(file))
}

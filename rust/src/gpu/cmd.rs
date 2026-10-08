//! External commands (cardwire, nvidia-smi, systemctl, journalctl). Behind a trait so tests can
//! check which commands would run - above all that nothing wakes a sleeping dGPU.

use std::env;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const POLL_TIMEOUT: Duration = Duration::from_secs(5);

pub trait Cmd {
    /// stdout without surrounding whitespace; "" on any failure or after the timeout.
    fn run(&self, args: &[&str]) -> String;
    fn which(&self, name: &str) -> bool;
}

pub struct System;

impl Cmd for System {
    fn run(&self, args: &[&str]) -> String {
        run_with_timeout(args, POLL_TIMEOUT)
    }

    fn which(&self, name: &str) -> bool {
        which(name)
    }
}

pub fn which(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else { return false };
    env::split_paths(&path).any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(path: &Path) -> bool {
    path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub fn run_with_timeout(args: &[&str], timeout: Duration) -> String {
    let Some((prog, rest)) = args.split_first() else { return String::new() };
    let Ok(mut child) =
        Command::new(prog).args(rest).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
    else {
        return String::new();
    };
    // Read in a thread so a large output cannot fill the pipe and block the child.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return String::new();
            }
        }
    }
    String::from_utf8_lossy(&reader.join().unwrap_or_default()).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_trimmed_stdout() {
        assert_eq!(run_with_timeout(&["echo", " hi "], POLL_TIMEOUT), "hi");
        assert_eq!(run_with_timeout(&["/nonexistent/program"], POLL_TIMEOUT), "");
    }

    #[test]
    fn run_gives_up_after_the_timeout() {
        let start = Instant::now();
        assert_eq!(run_with_timeout(&["sleep", "5"], Duration::from_millis(200)), "");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

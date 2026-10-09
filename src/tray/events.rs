//! Background sources that wake the tray up: the kernel log (lost GPUs), cardwired's D-Bus signals
//! and udev PCI events. Each runs in its own thread and hands over to the UI thread.

use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::thread;

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::MatchRule;

use crate::gpu::cardwire_dbus::ROOT;
use crate::gpu::journal::GPU_LOST_RE;

/// Follow the kernel log for lost GPUs, from the start of this boot. Needs read access to the
/// system journal (groups wheel, adm or systemd-journal); without it the tray relies on sysfs.
pub fn kernel_log(on_line: impl Fn(String) + Send + 'static) {
    thread::spawn(move || {
        let Ok(mut child) = Command::new("journalctl")
            .args(["-k", "-b", "-f", "-n", "all", "-o", "short-unix", "--no-pager", "--grep", GPU_LOST_RE])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let out = BufReader::new(child.stdout.take().expect("stdout is piped"));
        for line in out.lines().map_while(Result::ok) {
            on_line(line);
        }
        let _ = child.wait();
    });
}

/// Any signal under cardwired's object tree (mode, block and power state changes, GPUs added or
/// removed). Matched by path, not by sender, so a restarted cardwired is still heard.
pub fn cardwire_signals(on_signal: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        let run = || -> zbus::Result<()> {
            let conn = Connection::system()?;
            let rule = MatchRule::builder().msg_type(Type::Signal).path_namespace(ROOT)?.build();
            for msg in MessageIterator::for_match_rule(rule, &conn, Some(64))? {
                if msg.is_ok() {
                    on_signal();
                }
            }
            Ok(())
        };
        let _ = run();
    });
}

/// PCI devices added, removed or rebound (XG Mobile connected, live switch, cardwire rescans).
pub fn udev_pci(on_event: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        let Ok(socket) = udev::MonitorBuilder::new().and_then(|b| b.match_subsystem("pci")).and_then(|b| b.listen())
        else {
            return;
        };
        let mut fds = [libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
        loop {
            // SAFETY: one valid pollfd for the monitor socket, which outlives the call.
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, -1) } < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if socket.iter().count() > 0 {
                on_event();
            }
        }
    });
}

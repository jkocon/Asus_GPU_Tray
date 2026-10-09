//! Fakes for the root helpers' tests.

use std::cell::RefCell;
use std::fs;
use std::time::Duration;

use super::Ops;
use crate::gpu::paths::Paths;

pub fn attr(paths: &Paths, name: &str, value: &str) {
    let dir = paths.attr.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("current_value"), format!("{value}\n")).unwrap();
}

pub fn nvidia_function(paths: &Paths, addr: &str) {
    let dir = paths.pci.join(addr);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("vendor"), "0x10de\n").unwrap();
}

/// Records what would have happened, in order. Commands answer from `answers` (the first entry
/// whose prefix matches the command line wins); anything else fails as if missing.
#[derive(Default)]
pub struct FakeOps {
    pub kernel_lost: bool,
    pub log: RefCell<Vec<String>>,
    pub answers: RefCell<Vec<(String, Option<String>)>>,
    /// Output lines and exit code of a streamed command.
    pub streamed: (Vec<String>, i32),
    /// What the streamed command does to the (fake) system.
    pub on_stream: Option<Box<dyn Fn()>>,
}

impl FakeOps {
    pub fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    /// The commands run, in order (without the other events).
    pub fn commands(&self) -> Vec<String> {
        self.log().into_iter().filter_map(|l| l.strip_prefix("run ").map(str::to_string)).collect()
    }

    pub fn answer(&self, prefix: &str, out: Option<&str>) {
        self.answers.borrow_mut().push((prefix.into(), out.map(str::to_string)));
    }
}

impl Ops for FakeOps {
    fn say(&self, msg: &str) {
        self.log.borrow_mut().push(format!("say {msg}"));
    }
    fn kernel_lost_gpu(&self) -> bool {
        self.kernel_lost
    }
    fn sync(&self) {
        self.log.borrow_mut().push("sync".into());
    }
    fn sleep(&self, _: Duration) {}
    fn sysrq(&self, _: &Paths, key: char) {
        self.log.borrow_mut().push(format!("sysrq {key}"));
    }
    fn reboot(&self) -> u8 {
        self.log.borrow_mut().push("reboot".into());
        0
    }
    fn run(&self, args: &[&str]) -> Option<String> {
        let line = args.join(" ");
        self.log.borrow_mut().push(format!("run {line}"));
        let answers = self.answers.borrow();
        answers.iter().find(|(prefix, _)| line.starts_with(prefix.as_str())).and_then(|(_, out)| out.clone())
    }
    fn stream(&self, args: &[&str], on_line: &mut dyn FnMut(&str)) -> i32 {
        self.log.borrow_mut().push(format!("stream {}", args.join(" ")));
        for line in &self.streamed.0 {
            on_line(line);
        }
        if let Some(f) = &self.on_stream {
            f();
        }
        self.streamed.1
    }
}

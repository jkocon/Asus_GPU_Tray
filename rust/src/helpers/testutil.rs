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

/// Records what would have happened, in order.
#[derive(Default)]
pub struct FakeOps {
    pub kernel_lost: bool,
    pub log: RefCell<Vec<String>>,
}

impl FakeOps {
    pub fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
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
}

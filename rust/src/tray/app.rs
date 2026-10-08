//! The tray logic, GpuTray of asus_gpu_tray.py, running on the slint event loop. Dialogs are async:
//! each user-facing flow is a future on that loop, so it reads top to bottom like the Python code.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::dialogs::{self, ask, message, undock_hint, Kind, SwitchWin, BUILTIN, XG};
use super::icon;
use super::menu::{self, Action, Context, TrayModel};
use super::notify::Notifier;
use super::settings::Settings;
use crate::gpu::cardwire::{Cardwire, CwDevices};
use crate::gpu::cardwire_dbus::DbusCardwire;
use crate::gpu::cmd::{run_checked, Cmd, System};
use crate::gpu::i18n::{capitalize_first, tr, trf};
use crate::gpu::journal::{last_live_switch, lost_gpus, parse_gpu_lost};
use crate::gpu::labels::{cw_label, describe, hw_label, xg_line};
use crate::gpu::paths::{read, Paths};
use crate::gpu::pci::Gpu;
use crate::gpu::procs::{
    card_holders, describe_procs, gpu_users, nvidia_nodes, stop_processes, user_processes, Proc, RESTARTABLE_APPS,
};
use crate::gpu::state::{
    can_switch_live, dgpu_missing, on_battery, read_state, working_gpu, xg_gone, xg_unlocked, CwRepair, GpuState,
    REBOOT_MODES,
};
use crate::gpu::stats::gpu_stats;

pub const POLL: Duration = Duration::from_secs(3);
/// After an unlock, how long to wait for the firmware: it removes the GPU within ~1 s and, after a
/// clean release, switches back to the built-in dGPU within another second.
const UNDOCK_WAIT_S: u32 = 5;
/// How long the built-in dGPU must be awake before the wake notification (two polls in Python).
const WAKE_AFTER: Duration = Duration::from_secs(6);
/// Never offered for killing when they hold the card: the desktop session would go down with them.
const SESSION_PROCESSES: &[&str] =
    &["kwin_wayland", "kwin_x11", "Xwayland", "Xorg", "gnome-shell", "plasmashell", "systemd"];

fn xg_gone_text() -> String {
    tr("The XG Mobile was disconnected while it was the active GPU. Unlocking it disconnects the GPU \
        at once, and it stays gone until a reboot, also after you connect it again. Apps that were \
        using it may stop responding.\n\n\
        Next time, switch to the built-in dGPU in this menu before unlocking the XG Mobile. (Unlocking \
        works without a reboot only when nothing at all uses the GPU, system services included.)")
}

/// The NVIDIA driver wedges after losing a GPU, and a normal shutdown then hangs in it.
fn emergency_reboot_text() -> String {
    tr("A normal shutdown would hang in the NVIDIA driver, so the computer restarts the emergency way: \
        the disks are synced and remounted read-only, then it resets at once. Open apps are not asked \
        to quit - save your work first.")
}

fn session_holds_text() -> String {
    tr("These cannot be closed without ending the session. See the KDE Plasma setup in the README.")
}

fn restartable_names() -> Vec<&'static str> {
    RESTARTABLE_APPS.iter().map(|(n, _)| *n).collect()
}

fn is_restartable(p: &Proc) -> bool {
    restartable_names().contains(&p.name().as_str())
}

fn is_session(p: &Proc) -> bool {
    SESSION_PROCESSES.contains(&p.name().as_str())
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

fn mtime(path: &std::path::Path) -> Option<f64> {
    let t = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs_f64())
}

fn spawn(fut: impl Future<Output = ()> + 'static) {
    let _ = slint::spawn_local(fut);
}

/// Start a program in its own session and reap it when it exits.
fn spawn_detached(cmd: &[String]) {
    let Some((prog, args)) = cmd.split_first() else { return };
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = c.spawn() {
        thread::spawn(move || child.wait());
    }
}

/// cardwired's answers, kept until one of its signals (or udev, or the user) says they changed.
struct CachedCardwire {
    inner: DbusCardwire,
    devices: RefCell<Option<CwDevices>>,
    mode: RefCell<Option<(String, Vec<String>)>>,
}

impl CachedCardwire {
    fn invalidate(&self) {
        *self.devices.borrow_mut() = None;
        *self.mode.borrow_mut() = None;
    }
}

impl Cardwire for CachedCardwire {
    fn devices(&self) -> Option<CwDevices> {
        if let Some(d) = &*self.devices.borrow() {
            return Some(d.clone());
        }
        let d = self.inner.devices();
        self.devices.borrow_mut().clone_from(&d); // not running is never cached
        d
    }

    fn mode(&self) -> (String, Vec<String>) {
        if let Some(m) = &*self.mode.borrow() {
            return m.clone();
        }
        let m = self.inner.mode();
        if !m.0.is_empty() {
            *self.mode.borrow_mut() = Some(m.clone());
        }
        m
    }

    fn refresh_gpu(&self) {
        self.inner.refresh_gpu();
        self.invalidate();
    }
}

struct LiveRun {
    mode: String,
    /// None until the unit is started (the progress window paints first).
    child: Option<Child>,
    started: f64,
}

#[derive(Default)]
struct St {
    state: Option<GpuState>,
    lost: Vec<Gpu>,
    live: Option<LiveRun>,
    restart_after_live: Vec<Vec<String>>,
    progress: Option<SwitchWin>,
    live_retries: u32,
    /// XG Mobile unlocked or gone while active; None before the first poll
    xg_problem: Option<bool>,
    undock_pending: bool,
    /// seconds waited for the firmware after an unlock
    undock_wait: u32,
    /// None before the first poll: no question at startup
    xg_was_locked: Option<bool>,
    dock_pending: bool,
    /// PCI address -> live numbers, filled when the menu opens
    gpu_stats: HashMap<String, String>,
    awake_since: Option<Instant>,
    wake_checked: bool,
    /// app names -> when they were reported
    wake_told: HashMap<Vec<String>, Instant>,
    /// PCI slot -> time of the last "GPU lost" kernel message
    lost_events: HashMap<String, f64>,
    lost_asked: HashSet<String>,
    repair: CwRepair,
    refresh_queued: bool,
}

pub struct App {
    paths: Paths,
    cardwire: CachedCardwire,
    settings: Settings,
    notifier: Notifier,
    tray: ksni::blocking::Handle<TrayModel>,
    live_timer: slint::Timer,
    st: RefCell<St>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

/// Called by ksni (its own thread) for menu items and clicks on the icon.
pub fn dispatch(action: Action) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app() {
            app.handle(action);
        }
    });
}

/// Called by the event threads: something changed, look again soon.
pub fn changed() {
    let _ = slint::invoke_from_event_loop(|| {
        if let Some(app) = app() {
            app.cardwire.invalidate();
            app.refresh_soon();
        }
    });
}

pub fn kernel_line(line: String) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app() {
            if let Some((slot, t)) = parse_gpu_lost(&line) {
                let mut st = app.st.borrow_mut();
                let e = st.lost_events.entry(slot).or_insert(0.0);
                *e = e.max(t);
                drop(st);
                app.refresh_soon();
            }
        }
    });
}

impl App {
    pub fn install(paths: Paths, tray: ksni::blocking::Handle<TrayModel>) -> Rc<App> {
        let app = Rc::new(App {
            paths,
            cardwire: CachedCardwire {
                inner: DbusCardwire::connect(),
                devices: RefCell::new(None),
                mode: RefCell::new(None),
            },
            settings: Settings::new(),
            notifier: Notifier::new(),
            tray,
            live_timer: slint::Timer::default(),
            st: RefCell::new(St::default()),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        app
    }

    pub fn tick() {
        if let Some(app) = app() {
            app.refresh();
        }
    }

    fn handle(self: &Rc<Self>, action: Action) {
        let me = self.clone();
        match action {
            Action::CwMode(mode) => spawn(async move { me.switch_cw(mode).await }),
            Action::HwMode(mode) => spawn(async move { me.switch_hw(mode).await }),
            Action::UndockNow => spawn(async move { me.undock_now().await }),
            Action::BringBackDgpu => self.start_live("Hybrid"),
            Action::RebootXgGone => spawn(async move {
                let s = me.current_state();
                me.offer_reboot(&s, "Hybrid", &xg_gone_text()).await;
            }),
            Action::RebootLost(addrs) => {
                let gpus: Vec<Gpu> =
                    self.st.borrow().lost.iter().filter(|g| addrs.contains(&g.addr)).cloned().collect();
                if !gpus.is_empty() {
                    spawn(async move { me.ask_reboot_lost(gpus).await });
                }
            }
            Action::NotifyWake(on) => {
                self.settings.set_notify_wake(on);
                self.force_refresh();
            }
            Action::Refresh => self.force_refresh(),
            Action::Quit => {
                self.tray.shutdown();
                let _ = slint::quit_event_loop();
            }
            Action::MenuShow => self.on_menu_show(),
            Action::Activate => self.on_activated(),
        }
    }

    fn read(&self) -> GpuState {
        let mut st = self.st.borrow_mut();
        read_state(&self.paths, &self.cardwire, &System, &mut st.repair)
    }

    fn current_state(&self) -> GpuState {
        let cached = self.st.borrow().state.clone();
        cached.unwrap_or_else(|| self.read())
    }

    fn switching(&self) -> bool {
        self.st.borrow().live.is_some()
    }

    fn notify(&self, summary: &str, body: &str, timeout_ms: i32) {
        self.notifier.show(summary, body, timeout_ms, false);
    }

    fn refresh_soon(self: &Rc<Self>) {
        let mut st = self.st.borrow_mut();
        if st.refresh_queued {
            return;
        }
        st.refresh_queued = true;
        drop(st);
        slint::Timer::single_shot(Duration::from_millis(150), || {
            if let Some(app) = app() {
                app.st.borrow_mut().refresh_queued = false;
                app.refresh();
            }
        });
    }

    fn force_refresh(self: &Rc<Self>) {
        self.st.borrow_mut().state = None;
        self.cardwire.invalidate();
        self.refresh();
    }

    fn refresh(self: &Rc<Self>) {
        let s = self.read();
        self.check_wake(&s);
        let switching = self.switching();
        // During a live switch the cards are removed and re-created on purpose.
        let lost: Vec<Gpu> = if switching {
            Vec::new()
        } else {
            let st = self.st.borrow();
            lost_gpus(&s, &st.lost_events, last_live_switch(&self.paths)).into_iter().cloned().collect()
        };
        self.watch(&s, &lost);
        let mut st = self.st.borrow_mut();
        if st.state.as_ref() == Some(&s) && st.lost == lost {
            return;
        }
        let alert = !lost.is_empty() || (!switching && (xg_unlocked(&s) || xg_gone(&s) || dgpu_missing(&s)));
        let icon = icon::sni_icon(working_gpu(&s), s.egpu().is_some(), alert);
        let mut tip = vec![format!("GPU: {}", describe(&s))];
        tip.extend(lost.iter().map(|g| trf("{name} fell off the bus – reboot needed", &[("name", &g.name)])));
        if s.cardwire {
            tip.push(trf("cardwire mode: {mode}", &[("mode", &capitalize_first(&s.cw_mode))]));
        }
        if s.asus_egpu {
            tip.push(xg_line(&s));
        }
        if let Some(run) = &st.live {
            tip.push(trf("Switching to {target}…", &[("target", &hw_label(&run.mode, &s))]));
        }
        if !s.hw_pending.is_empty() {
            tip.push(trf("After reboot: {mode}", &[("mode", &hw_label(&s.hw_pending, &s))]));
        }
        let items = menu::build(
            &s,
            &Context { lost: &lost, gpu_stats: &st.gpu_stats, switching, notify_wake: self.settings.notify_wake() },
        );
        st.state = Some(s);
        st.lost = lost;
        drop(st);
        let tooltip = tip.join("\n");
        self.tray.update(move |t| {
            t.items = items;
            t.icon = vec![icon];
            t.tooltip = tooltip;
        });
    }

    /// On battery: say which apps woke the built-in dGPU once it has been awake for ~6 s. Each set
    /// of apps at most every 10 minutes.
    fn check_wake(&self, s: &GpuState) {
        let g = s.dgpu().filter(|g| g.power == "active" && !g.blocked && !g.driver.is_empty());
        let mut st = self.st.borrow_mut();
        let Some(g) = g else {
            st.awake_since = None;
            st.wake_checked = false;
            return;
        };
        let since = *st.awake_since.get_or_insert_with(Instant::now);
        if st.wake_checked || since.elapsed() < WAKE_AFTER {
            return;
        }
        st.wake_checked = true;
        if !self.settings.notify_wake() || !on_battery(&self.paths) {
            return;
        }
        let mut names: Vec<String> = gpu_users(&self.paths, g).iter().map(Proc::name).collect();
        names.sort();
        names.dedup();
        if names.is_empty() || st.wake_told.get(&names).is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
            return;
        }
        st.wake_told.insert(names.clone(), Instant::now());
        drop(st);
        self.notify(
            &trf("{name} woke up", &[("name", &g.name)]),
            &trf("On battery, used by: {apps}", &[("apps", &names.join(", "))]),
            8000,
        );
    }

    /// React to the XG Mobile being locked or unlocked and to lost GPUs - once no dialog is open.
    fn watch(self: &Rc<Self>, s: &GpuState, lost: &[Gpu]) {
        let problem = xg_unlocked(s) || xg_gone(s) || dgpu_missing(s);
        let switching = self.switching();
        let mut st = self.st.borrow_mut();
        if st.xg_problem == Some(false) && problem && !switching {
            st.undock_pending = true;
        }
        if !problem {
            st.undock_pending = false;
        }
        st.xg_problem = Some(problem);
        // Dock connected and locked while on the built-in dGPU: offer to switch to it.
        if st.xg_was_locked == Some(false) && s.egpu_connected && !switching {
            st.dock_pending = true;
        }
        if !s.egpu_connected || s.hw_mode != "Hybrid" {
            st.dock_pending = false;
        }
        st.xg_was_locked = Some(s.egpu_connected);
        if switching || dialogs::any_open() {
            return;
        }
        let new_lost: Vec<Gpu> = lost.iter().filter(|g| !st.lost_asked.contains(&g.addr)).cloned().collect();
        let me = self.clone();
        if !new_lost.is_empty() {
            st.lost_asked.extend(new_lost.iter().map(|g| g.addr.clone()));
            spawn(async move { me.ask_reboot_lost(new_lost).await });
        } else if st.undock_pending {
            st.undock_pending = false;
            spawn(async move { me.on_xg_unlocked().await });
        } else if st.dock_pending {
            st.dock_pending = false;
            spawn(async move { me.on_xg_locked().await });
        }
    }

    fn on_menu_show(self: &Rc<Self>) {
        let s = self.read();
        let stats = s.gpus.iter().map(|g| (g.addr.clone(), gpu_stats(g, &System))).collect();
        self.st.borrow_mut().gpu_stats = stats;
        self.force_refresh(); // open the menu with the current state
    }

    fn on_activated(self: &Rc<Self>) {
        self.refresh();
        let s = self.current_state();
        let mut text = describe(&s);
        if let Some(stats) = working_gpu(&s).map(|g| gpu_stats(g, &System)).filter(|t| !t.is_empty()) {
            text += &format!("\n{stats}");
        }
        self.notify(&tr("Active GPU"), &text, 4000);
    }

    // --- cardwire ------------------------------------------------------------------------------

    async fn switch_cw(self: Rc<Self>, mode: String) {
        let s = self.current_state();
        if mode == s.cw_mode || self.switching() {
            return self.force_refresh(); // clicked the current mode - just restore the check mark
        }
        match run_checked(&["cardwire", "set", &mode]) {
            Err(e) => {
                message(Kind::Critical, &tr("Change GPU mode"), &trf("cardwire failed:\n{error}", &[("error", &e)]))
                    .await
            }
            Ok(()) => {
                let note = if mode == "hybrid" {
                    String::new()
                } else {
                    format!("\n{}", tr("Apps already running keep their GPU until restarted."))
                };
                self.notify(&tr("GPU mode"), &format!("{}{note}", cw_label(&mode, &s)), 4000);
            }
        }
        self.force_refresh();
    }

    // --- hardware (ASUS) ------------------------------------------------------------------------

    async fn switch_hw(self: Rc<Self>, mode: String) {
        let s = self.current_state();
        let current = if s.hw_pending.is_empty() { &s.hw_mode } else { &s.hw_pending };
        if &mode == current || self.switching() {
            return self.force_refresh();
        }
        if mode == "AsusEgpu" && self.paths.read_attr("egpu_connected") != "1" {
            message(Kind::Warning, XG, &tr("XG Mobile is not connected and locked.")).await;
            return self.force_refresh();
        }
        if can_switch_live(&s, &mode) {
            return self.switch_hw_live(mode, String::new()).await;
        }
        if !s.reboot_backend {
            message(
                Kind::Warning,
                &tr("Change GPU mode"),
                &tr("The reboot-based switch backend is not installed. Run install.sh as root."),
            )
            .await;
            return self.force_refresh();
        }
        if self.confirm_reboot(&hw_label(&mode, &s)).await {
            self.start_reboot_switch(&mode).await;
        }
        self.force_refresh();
    }

    /// Live switch built-in dGPU <-> XG Mobile. intro goes before the question.
    async fn switch_hw_live(self: Rc<Self>, mode: String, intro: String) {
        let s = self.current_state();
        self.st.borrow_mut().live_retries = 0;
        let lost = self.st.borrow().lost.clone();
        if !lost.is_empty() || xg_gone(&s) || xg_unlocked(&s) {
            // A lost GPU, or one the firmware is removing: the NVIDIA driver is (about to be) wedged,
            // and the live switch would hang in it. Only a reboot gets out of this.
            let why = if lost.is_empty() {
                xg_gone_text()
            } else {
                let names: Vec<&str> = lost.iter().map(|g| g.name.as_str()).collect();
                trf("{name} is lost, so the switch needs a reboot.", &[("name", &names.join(", "))])
            };
            self.offer_reboot(&s, &mode, &why).await;
            return self.force_refresh();
        }
        let mut text = intro.clone()
            + &trf("Switch to: {target}?", &[("target", &hw_label(&mode, &s))])
            + "\n\n"
            + &tr("No reboot needed; it takes about 40 seconds. Apps that use the NVIDIA GPU will be \
                   listed first, so you can close them.");
        if !user_processes(&restartable_names()).is_empty() {
            text += &format!("\n\n{}", tr("ROG Control Center will be closed and started again afterwards."));
        }
        let title = if intro.is_empty() { tr("Change GPU mode") } else { tr("XG Mobile connected") };
        if !ask(&title, &text).await {
            return self.force_refresh();
        }
        if self.free_card(&s, &mode).await {
            self.start_live(&mode);
        }
        self.force_refresh();
    }

    /// One confirmation, then close every app that has the XG Mobile's GPU open and switch to the
    /// built-in dGPU live, so the dock can be unplugged. It must happen before the XG Mobile is
    /// unlocked: afterwards the firmware has already dropped the GPU, and closing the apps would
    /// hang in the driver.
    async fn undock_now(self: Rc<Self>) {
        let s = self.current_state();
        if self.switching() {
            return;
        }
        self.st.borrow_mut().live_retries = 0;
        if !self.st.borrow().lost.is_empty() || xg_gone(&s) || xg_unlocked(&s) {
            self.offer_reboot(&s, "Hybrid", &xg_gone_text()).await;
            return self.force_refresh();
        }
        let title = tr("Undock now");
        let procs: Vec<Proc> =
            card_holders(&nvidia_nodes(&self.paths), false).into_iter().filter(|p| !is_restartable(p)).collect();
        let session: Vec<Proc> = procs.iter().filter(|p| is_session(p)).cloned().collect();
        if !session.is_empty() {
            let text = trf("The desktop session itself uses the {gpu}:", &[("gpu", XG)])
                + &format!("\n\n{}\n\n", describe_procs(&session))
                + &session_holds_text();
            message(Kind::Warning, &title, &text).await;
            self.offer_reboot(&s, "Hybrid", "").await;
            return self.force_refresh();
        }
        let mut text = tr("Switch to the built-in dGPU now, so the XG Mobile can be unplugged?") + "\n\n";
        if !procs.is_empty() {
            text += &tr("These apps use the XG Mobile and will be closed without asking again; unsaved work \
                         in them is lost:");
            text += &format!("\n\n{}\n\n", describe_procs(&procs));
        }
        text += &tr("ROG Control Center is closed and started again, and the GPU services are stopped during \
                     the switch. It takes about 35 seconds. Keep the XG Mobile locked until it is done.");
        if !ask(&title, &text).await {
            return self.force_refresh();
        }
        // Again: apps may have opened the GPU while the dialog was up.
        let procs: Vec<Proc> = card_holders(&nvidia_nodes(&self.paths), false)
            .into_iter()
            .filter(|p| !is_restartable(p) && !is_session(p))
            .collect();
        stop_processes(&procs, Duration::from_secs(5));
        let left: Vec<Proc> = procs.into_iter().filter(Proc::alive).collect();
        if !left.is_empty() {
            message(Kind::Warning, &title, &format!("{}\n\n{}", tr("These apps did not quit:"), describe_procs(&left)))
                .await;
            self.offer_reboot(&s, "Hybrid", "").await;
            return self.force_refresh();
        }
        self.start_live("Hybrid");
        self.force_refresh();
    }

    /// True when none of this user's apps (except restartable ones) holds the NVIDIA card any
    /// more. Otherwise lists them and offers to kill them; if the user says no, offers a reboot.
    async fn free_card(&self, s: &GpuState, mode: &str) -> bool {
        let procs: Vec<Proc> =
            card_holders(&nvidia_nodes(&self.paths), false).into_iter().filter(|p| !is_restartable(p)).collect();
        if procs.is_empty() {
            return true;
        }
        let name = match s.egpu().or(s.dgpu()) {
            Some(g) => format!("{} ({})", g.name, g.kind.label()),
            None => tr("NVIDIA GPU"),
        };
        let title = tr("GPU in use");
        let session: Vec<Proc> = procs.iter().filter(|p| is_session(p)).cloned().collect();
        if !session.is_empty() {
            let text = trf("The desktop session itself uses the {gpu}:", &[("gpu", &name)])
                + &format!("\n\n{}\n\n", describe_procs(&session))
                + &session_holds_text();
            message(Kind::Warning, &title, &text).await;
        } else {
            let text = trf("These apps are using the {gpu}:", &[("gpu", &name)])
                + &format!("\n\n{}\n\n", describe_procs(&procs))
                + &tr("Kill them to continue the switch? Unsaved work in them will be lost.");
            if ask(&title, &text).await {
                stop_processes(&procs, Duration::from_secs(5));
                let left: Vec<Proc> = procs.into_iter().filter(Proc::alive).collect();
                if left.is_empty() {
                    return true;
                }
                message(
                    Kind::Warning,
                    &title,
                    &format!("{}\n\n{}", tr("These apps did not quit:"), describe_procs(&left)),
                )
                .await;
            }
        }
        self.offer_reboot(s, mode, "").await;
        false
    }

    /// Ask to reboot and switch during boot instead of live.
    async fn offer_reboot(&self, s: &GpuState, mode: &str, why: &str) {
        let why = if why.is_empty() { String::new() } else { format!("{why}\n\n") };
        if !s.reboot_backend {
            let text = why + &tr("The reboot-based switch backend is not installed.");
            return message(Kind::Warning, &tr("Change GPU mode"), &text).await;
        }
        let mut text = why
            + &trf("Reboot now and switch to {target} during boot?", &[("target", &hw_label(mode, s))])
            + "\n\n"
            + &tr("Save your work first: the computer restarts right away.");
        if !self.st.borrow().lost.is_empty() || xg_gone(s) || xg_unlocked(s) {
            text += &format!("\n\n{}", emergency_reboot_text());
        } else if s.hw_mode == "AsusEgpu" {
            text += &format!("\n\n{}", tr("Disconnect the XG Mobile only once the computer has restarted."));
        }
        if ask(&tr("Reboot"), &text).await {
            self.start_reboot_switch(mode).await;
        }
    }

    /// The XG Mobile was connected and locked while the built-in dGPU is active: offer the live
    /// switch to it (the same dialogs as from the menu).
    async fn on_xg_locked(self: Rc<Self>) {
        let s = self.read();
        let lost = !self.st.borrow().lost.is_empty();
        if self.switching() || !s.egpu_connected || s.hw_mode != "Hybrid" || lost || dgpu_missing(&s) {
            return;
        }
        if !can_switch_live(&s, "AsusEgpu") {
            return; // no live backend, or a reboot switch is pending: the menu still offers what works
        }
        self.st.borrow_mut().state = Some(s);
        let intro = tr("The XG Mobile is connected and locked.") + "\n\n";
        self.switch_hw_live("AsusEgpu".into(), intro).await;
    }

    /// The XG Mobile was unlocked while it was the active GPU. The firmware removes its GPU at
    /// once (0.1-1.1 s on a GV601RE), so there is never time for a live switch. What follows tells
    /// the two outcomes apart:
    /// - nothing held the GPU, the driver let it go, and the firmware switched back to the built-in
    ///   dGPU by itself: only the link and a rescan are missing - done live, no reboot;
    /// - something held it: egpu_enable stays 1 and the NVIDIA driver is wedged - reboot.
    async fn on_xg_unlocked(self: Rc<Self>) {
        let s = self.read();
        if self.switching() {
            return;
        }
        if dgpu_missing(&s) && s.live_backend {
            self.st.borrow_mut().undock_wait = 0;
            self.notify(&tr("XG Mobile released"), &tr("Bringing back the built-in dGPU…"), 5000);
            return self.start_live("Hybrid");
        }
        if !(xg_gone(&s) || xg_unlocked(&s)) {
            self.st.borrow_mut().undock_wait = 0;
            return;
        }
        let waited = self.st.borrow().undock_wait;
        if waited < UNDOCK_WAIT_S {
            self.st.borrow_mut().undock_wait += 1;
            slint::Timer::single_shot(Duration::from_secs(1), || {
                if let Some(app) = app() {
                    spawn(async move { app.on_xg_unlocked().await });
                }
            });
            return;
        }
        self.st.borrow_mut().undock_wait = 0;
        self.notifier.show(
            &tr("XG Mobile disconnected"),
            &tr("It was unlocked while in use - a reboot is needed."),
            10000,
            true,
        );
        let mut why = xg_gone_text();
        // The apps still have the dead GPU open; services such as cardwired and nvidia-powerd do
        // too, but run as root and cannot be seen from here.
        let procs = card_holders(&nvidia_nodes(&self.paths), false);
        if !procs.is_empty() {
            why += &format!("\n\n{}\n{}", tr("Still holding the GPU:"), describe_procs(&procs));
        }
        why += &format!("\n\n{}", tr("Without a reboot the built-in dGPU stays unavailable until the next boot."));
        self.offer_reboot(&s, "Hybrid", &why).await;
        self.force_refresh();
    }

    fn start_live(self: &Rc<Self>, mode: &str) {
        let s = self.current_state();
        let (old, new) = if mode == "AsusEgpu" { (BUILTIN, XG) } else { (XG, BUILTIN) };
        let win = SwitchWin::show(&hw_label(mode, &s), old, new);
        {
            let mut st = self.st.borrow_mut();
            st.progress = win;
            st.live = Some(LiveRun { mode: mode.into(), child: None, started: now() });
        }
        // Paint the window before closing ROG Control Center blocks for a moment.
        let mode = mode.to_string();
        slint::Timer::single_shot(Duration::from_millis(100), move || {
            if let Some(app) = app() {
                app.start_live_unit(&mode);
            }
        });
        self.force_refresh();
    }

    fn start_live_unit(self: &Rc<Self>, mode: &str) {
        let apps = user_processes(&restartable_names()); // now: the dialogs may have been open for a while
        stop_processes(&apps, Duration::from_secs(5));
        let started = now();
        let child = Command::new("systemctl")
            .args(["start", &format!("asus-gpu-live@{mode}.service")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        {
            let mut st = self.st.borrow_mut();
            st.restart_after_live = apps.iter().map(Proc::restart_cmd).collect();
            if let Some(run) = st.live.as_mut() {
                run.started = started;
                run.child = child.ok();
            }
        }
        self.live_timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), || {
            if let Some(app) = app() {
                app.check_live_switch();
            }
        });
    }

    /// The last line of live-progress, if it belongs to the switch that is running.
    fn live_step(&self, started: f64) -> String {
        if mtime(&self.paths.live_progress).is_none_or(|t| t < started - 1.0) {
            return String::new();
        }
        read(&self.paths.live_progress).lines().last().unwrap_or("").to_string()
    }

    fn check_live_switch(self: &Rc<Self>) {
        let finished = {
            let mut st = self.st.borrow_mut();
            let Some(run) = st.live.as_mut() else { return };
            match run.child.as_mut().map(Child::try_wait) {
                Some(Ok(None)) => None,
                Some(Ok(Some(status))) => Some(status.code().unwrap_or(-1)),
                // the unit could not even be started
                Some(Err(_)) | None => Some(-1),
            }
        };
        let Some(rc) = finished else {
            let st = self.st.borrow();
            if let (Some(win), Some(run)) = (&st.progress, &st.live) {
                win.update_progress(&self.live_step(run.started));
            }
            return;
        };
        self.live_timer.stop();
        let (run, restart) = {
            let mut st = self.st.borrow_mut();
            (st.live.take().expect("checked above"), std::mem::take(&mut st.restart_after_live))
        };
        let mut err = String::new();
        if let Some(mut child) = run.child {
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
        }
        for cmd in &restart {
            spawn_detached(cmd);
        }
        // The result file is left over from the previous switch if the unit never ran (polkit said no).
        let fresh = mtime(&self.paths.live_result).is_some_and(|t| t >= run.started - 1.0);
        let result = if fresh { read(&self.paths.live_result) } else { String::new() };
        let mut text = if !result.is_empty() {
            result
        } else if !err.trim().is_empty() {
            err.trim().to_string()
        } else {
            trf("systemctl exited with {code}", &[("code", &rc.to_string())])
        };
        if let Some(win) = &self.st.borrow().progress {
            win.finish(rc == 0);
        }
        let s = self.current_state();
        let retries = self.st.borrow().live_retries;
        let me = self.clone();
        if rc == 0 {
            let hint = if run.mode == "Hybrid" { tr("You can disconnect the XG Mobile now.") } else { undock_hint() };
            text += &format!("\n{hint}");
            self.notify(&tr("GPU switched"), &text, 10000);
        } else if text.starts_with("Aborted, the NVIDIA card is still in use") && retries < 2 {
            // Something opened the card after the check. Ask about this user's apps again.
            self.st.borrow_mut().live_retries += 1;
            spawn(async move {
                if card_holders(&nvidia_nodes(&me.paths), false).is_empty() {
                    me.offer_reboot(&s, &run.mode, &text).await;
                } else if me.free_card(&s, &run.mode).await {
                    me.start_live(&run.mode);
                }
                me.force_refresh();
            });
        } else {
            spawn(async move {
                me.offer_reboot(&s, &run.mode, &text).await;
                me.force_refresh();
            });
        }
        self.force_refresh();
    }

    async fn ask_reboot_lost(self: Rc<Self>, gpus: Vec<Gpu>) {
        let names: Vec<&str> = gpus.iter().map(|g| g.name.as_str()).collect();
        let s = self.current_state();
        // Through the reboot unit when there is one: it reboots the emergency way after a GPU loss.
        let mode = if xg_unlocked(&s) || xg_gone(&s) {
            "Hybrid".to_string()
        } else if s.hw_pending.is_empty() {
            s.hw_mode.clone()
        } else {
            s.hw_pending.clone()
        };
        let via_unit = s.reboot_backend && REBOOT_MODES.contains(&mode.as_str());
        let mut text = trf(
            "{names} fell off the PCIe bus. Apps can no longer use it, and it comes back only after a reboot.",
            &[("names", &names.join(", "))],
        ) + "\n\n"
            + &tr("Reboot now?");
        if via_unit {
            text += &format!("\n\n{}", emergency_reboot_text());
            if mode == "Hybrid" && s.hw_mode == "AsusEgpu" {
                text += &format!(
                    "\n\n{}",
                    tr("The XG Mobile was unlocked, so the computer will start on the built-in dGPU.")
                );
            }
        } else {
            text += &format!(" {}", tr("Save your work first."));
        }
        if !ask(&tr("GPU lost"), &text).await {
            return;
        }
        if via_unit {
            return self.start_reboot_switch(&mode).await;
        }
        if let Err(e) = run_checked(&["systemctl", "reboot"]) {
            message(Kind::Critical, &tr("Reboot"), &format!("{}\n{e}", tr("Reboot failed:"))).await;
        }
    }

    async fn confirm_reboot(&self, title: &str) -> bool {
        let mut text = trf("Switch to: {target}?", &[("target", title)])
            + "\n\n"
            + &tr("The computer will reboot right away and the change is applied during boot. Save your work.");
        if self.st.borrow().state.as_ref().is_some_and(|s| s.hw_mode == "AsusEgpu") {
            text += &format!("\n\n{}", tr("Disconnect the XG Mobile only once the new mode is active."));
        }
        ask(&tr("Change GPU mode"), &text).await
    }

    async fn start_reboot_switch(&self, mode: &str) {
        let unit = format!("asus-gpu-switch@{mode}.service");
        if let Err(e) = run_checked(&["systemctl", "start", &unit]) {
            return message(Kind::Critical, &tr("Change GPU mode"), &format!("{}\n{e}", tr("Switching failed:"))).await;
        }
        // The unit is Type=simple, so "start" succeeds even when the script refuses; the reboot
        // follows within a second when it does not.
        slint::Timer::single_shot(Duration::from_secs(3), move || {
            if System.run(&["systemctl", "is-failed", &unit]) != "failed" {
                return;
            }
            let why = System.run(&["journalctl", "-b", "-u", &unit, "-n", "1", "-o", "cat", "--no-pager"]);
            let why = if why.is_empty() { format!("{unit} failed") } else { why };
            spawn(async move {
                message(
                    Kind::Warning,
                    &tr("Change GPU mode"),
                    &format!("{}\n{why}", tr("The switch was not scheduled:")),
                )
                .await;
            });
        });
    }
}

//! The tray menu: built as plain data from the state (testable), shown through ksni (DBusMenu).

use std::collections::HashMap;

use crate::gpu::i18n::{tr, trf};
use crate::gpu::labels::{cw_label, gpu_line, hw_label, xg_line};
use crate::gpu::pci::Gpu;
use crate::gpu::state::{can_switch_live, dgpu_missing, hw_modes, xg_gone, GpuState};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    CwMode(String),
    HwMode(String),
    UndockNow,
    BringBackDgpu,
    RebootXgGone,
    RebootLost(Vec<String>),
    NotifyWake(bool),
    Refresh,
    Quit,
    /// From the tray host, not from a menu item.
    MenuShow,
    Activate,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Greyed-out text.
    Label(String),
    Button(String, Action),
    Separator,
    Section(String),
    /// (label, enabled, action) per option; `selected` out of range selects nothing.
    Radio {
        options: Vec<(String, bool, Action)>,
        selected: usize,
    },
    Check(String, bool, Action),
}

/// What the menu shows besides the GPU state.
pub struct Context<'a> {
    pub lost: &'a [Gpu],
    pub gpu_stats: &'a HashMap<String, String>,
    pub switching: bool,
    pub notify_wake: bool,
}

fn radio(
    items: &mut Vec<Item>,
    title: String,
    modes: &[String],
    current: &str,
    label: impl Fn(&str) -> String,
    action: impl Fn(&str) -> Action,
    disabled: impl Fn(&str) -> String,
) {
    items.push(Item::Section(title));
    let options = modes
        .iter()
        .map(|m| {
            let reason = disabled(m);
            if reason.is_empty() {
                (label(m), true, action(m))
            } else {
                (format!("{} – {reason}", label(m)), false, action(m))
            }
        })
        .collect();
    let selected = modes.iter().position(|m| m == current).unwrap_or(usize::MAX);
    items.push(Item::Radio { options, selected });
}

pub fn build(s: &GpuState, cx: &Context) -> Vec<Item> {
    let mut m = Vec::new();
    for g in &s.gpus {
        m.push(Item::Label(gpu_line(g)));
        if let Some(stats) = cx.gpu_stats.get(&g.addr).filter(|t| !t.is_empty()) {
            m.push(Item::Label(format!("    {stats}")));
        }
    }
    if s.gpus.is_empty() {
        m.push(Item::Label(tr("No graphics card detected")));
    }
    for g in cx.lost {
        m.push(Item::Button(
            trf("⚠ {name} fell off the bus – Reboot…", &[("name", &g.name)]),
            Action::RebootLost(vec![g.addr.clone()]),
        ));
    }
    if s.asus_egpu {
        m.push(Item::Label(xg_line(s)));
    }
    if xg_gone(s) && !cx.switching {
        m.push(Item::Button(tr("⚠ XG Mobile disconnected – Reboot…"), Action::RebootXgGone));
    }
    if dgpu_missing(s) && s.live_backend && !cx.switching {
        m.push(Item::Button(tr("⚠ Built-in dGPU missing – Bring it back"), Action::BringBackDgpu));
    }
    if !s.hw_pending.is_empty() {
        m.push(Item::Label(trf("After reboot: {mode}", &[("mode", &hw_label(&s.hw_pending, s))])));
    }

    let busy = |_: &str| if cx.switching { tr("switching…") } else { String::new() };
    let xg_disabled = |mode: &str| {
        if cx.switching {
            tr("switching…")
        } else if mode == "AsusEgpu" && !s.egpu_connected {
            tr("connect and lock the dock")
        } else if mode == "AsusMuxDgpu" && s.hw_mode == "AsusEgpu" {
            tr("switch to the built-in dGPU first") // the firmware refuses it (EBUSY)
        } else {
            String::new()
        }
    };
    let hw_current = if s.hw_pending.is_empty() { s.hw_mode.as_str() } else { s.hw_pending.as_str() };
    let hw_item = |mode: &str| {
        let current = mode == hw_current;
        let refused = mode == "AsusMuxDgpu" && s.hw_mode == "AsusEgpu"; // xg_disabled explains why
        if mode == "Hybrid" && s.hw_mode == "AsusEgpu" {
            return hw_label(mode, s) + &tr(" – before undocking");
        }
        let suffix = if current || refused || can_switch_live(s, mode) { String::new() } else { tr(" – reboot") };
        hw_label(mode, s) + &suffix
    };

    if s.cardwire && s.hw_mode == "AsusMuxDgpu" {
        // The panel is wired to the dGPU: cardwire's modes change nothing, and a checked "Hybrid"
        // read as if the laptop were in Hybrid. Going back is a hardware switch.
        m.push(Item::Section(tr("GPU access (live, cardwire)")));
        m.push(Item::Label(tr("Not used in the MUX mode – the dGPU drives the screen")));
    } else if s.cardwire {
        radio(
            &mut m,
            tr("GPU access (live, cardwire)"),
            &s.cw_modes,
            &s.cw_mode,
            |x| cw_label(x, s),
            |x| Action::CwMode(x.into()),
            busy,
        );
    }
    // The hardware switch needs only asus-armoury, not cardwire.
    if s.asus_egpu {
        let modes: Vec<String> = hw_modes(s).iter().map(|m| m.to_string()).collect();
        radio(&mut m, tr("Hardware"), &modes, hw_current, hw_item, |x| Action::HwMode(x.into()), xg_disabled);
    }
    if !s.cardwire && !s.asus_egpu {
        m.push(Item::Separator);
        m.push(Item::Label(tr("Switching unavailable (install cardwire)")));
    }
    if s.hw_mode == "AsusEgpu" && can_switch_live(s, "Hybrid") && !cx.switching {
        m.push(Item::Separator);
        m.push(Item::Button(tr("Undock now (close all GPU apps)…"), Action::UndockNow));
    }
    m.push(Item::Separator);
    if s.dgpu().is_some() {
        m.push(Item::Check(
            tr("Notify when the dGPU wakes up on battery"),
            cx.notify_wake,
            Action::NotifyWake(!cx.notify_wake),
        ));
    }
    m.push(Item::Button(tr("Refresh"), Action::Refresh));
    m.push(Item::Button(tr("Quit"), Action::Quit));
    m
}

/// Visible texts, for tests and logging.
pub fn texts(items: &[Item]) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Label(t) | Item::Button(t, _) | Item::Section(t) | Item::Check(t, _, _) => out.push(t.clone()),
            Item::Radio { options, .. } => out.extend(options.iter().map(|(t, _, _)| t.clone())),
            Item::Separator => {}
        }
    }
    out
}

/// What ksni shows; updated from the UI thread through the service handle.
pub struct TrayModel {
    pub items: Vec<Item>,
    pub icon: Vec<ksni::Icon>,
    pub tooltip: String,
    pub dispatch: fn(Action),
}

/// DBusMenu uses "_" for access keys; a literal one is doubled.
fn label(text: &str) -> String {
    text.replace('_', "__")
}

impl ksni::Tray for TrayModel {
    fn id(&self) -> String {
        "asus-gpu-tray".into()
    }

    fn title(&self) -> String {
        "Asus GPU Tray".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_name(&self) -> String {
        if self.icon.is_empty() {
            "asus-gpu-tray".into()
        } else {
            String::new()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let (title, description) = self.tooltip.split_once('\n').unwrap_or((&self.tooltip, ""));
        ksni::ToolTip { title: title.into(), description: description.into(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.dispatch)(Action::Activate);
    }

    fn menu_about_to_show(&mut self) {
        (self.dispatch)(Action::MenuShow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem};
        let dispatch = self.dispatch;
        let mut out = Vec::new();
        for item in &self.items {
            match item.clone() {
                Item::Label(t) => {
                    out.push(StandardItem { label: label(&t), enabled: false, ..Default::default() }.into())
                }
                Item::Section(t) => {
                    out.push(ksni::MenuItem::Separator);
                    out.push(StandardItem { label: label(&t), enabled: false, ..Default::default() }.into());
                }
                Item::Separator => out.push(ksni::MenuItem::Separator),
                Item::Button(t, action) => out.push(
                    StandardItem {
                        label: label(&t),
                        activate: Box::new(move |_: &mut Self| dispatch(action.clone())),
                        ..Default::default()
                    }
                    .into(),
                ),
                Item::Check(t, checked, action) => out.push(
                    CheckmarkItem {
                        label: label(&t),
                        checked,
                        activate: Box::new(move |_: &mut Self| dispatch(action.clone())),
                        ..Default::default()
                    }
                    .into(),
                ),
                Item::Radio { options, selected } => {
                    let actions: Vec<Action> = options.iter().map(|(_, _, a)| a.clone()).collect();
                    out.push(
                        RadioGroup {
                            selected,
                            select: Box::new(move |_: &mut Self, i| {
                                if let Some(a) = actions.get(i) {
                                    dispatch(a.clone());
                                }
                            }),
                            options: options
                                .into_iter()
                                .map(|(t, enabled, _)| RadioItem { label: label(&t), enabled, ..Default::default() })
                                .collect(),
                        }
                        .into(),
                    );
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::testutil::{dgpu, igpu, state};

    fn menu(s: &GpuState, switching: bool) -> Vec<String> {
        let stats = HashMap::from([("0000:01:00.0".to_string(), "54 °C · 38 W".to_string())]);
        texts(&build(s, &Context { lost: &[], gpu_stats: &stats, switching, notify_wake: true }))
    }

    #[test]
    fn xg_mobile_mode() {
        let t = menu(&state(), false);
        assert!(t.contains(&"    54 °C · 38 W".to_string()));
        assert!(t.contains(&"XG Mobile: connected".to_string()));
        assert!(t.contains(&"Built-in dGPU – before undocking".to_string()));
        assert!(t.contains(&"Integrated – block RTX 3070".to_string()));
        assert!(t.contains(&"Built-in dGPU only (MUX) – switch to the built-in dGPU first".to_string()));
        assert!(t.contains(&"Undock now (close all GPU apps)…".to_string()));
        assert!(!t.contains(&"Notify when the dGPU wakes up on battery".to_string()));
        // no built-in dGPU listed
    }

    #[test]
    fn switching_disables_the_modes() {
        let t = menu(&state(), true);
        assert!(t.contains(&"Hybrid – all GPUs available – switching…".to_string()));
        assert!(!t.contains(&"Undock now (close all GPU apps)…".to_string()));
    }

    #[test]
    fn built_in_dgpu_mode() {
        let s = GpuState { gpus: vec![dgpu(), igpu()], hw_mode: "Hybrid".into(), egpu_connected: false, ..state() };
        let t = menu(&s, false);
        assert!(t.contains(&"Built-in dGPU (RTX 3050 Ti)".to_string()));
        assert!(t.contains(&"XG Mobile – connect and lock the dock".to_string()));
        assert!(t.contains(&"RTX 3050 Ti only (MUX) – reboot".to_string()));
        assert!(t.contains(&"Notify when the dGPU wakes up on battery".to_string()));
    }

    #[test]
    fn mux_mode_has_no_cardwire_modes() {
        let s = GpuState { gpus: vec![dgpu(), igpu()], hw_mode: "AsusMuxDgpu".into(), egpu_connected: false, ..state() };
        let t = menu(&s, false);
        assert!(t.contains(&"Not used in the MUX mode – the dGPU drives the screen".to_string()));
        assert!(!t.iter().any(|l| l.starts_with("Hybrid – ")), "{t:?}");
        assert!(t.contains(&"Built-in dGPU (RTX 3050 Ti) – reboot".to_string()), "{t:?}");
    }
}

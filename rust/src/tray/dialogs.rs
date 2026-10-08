//! Message boxes and the live-switch progress window (slint, software renderer: showing them opens
//! no GPU device node, so they never get in the way of the switch).

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use crate::gpu::i18n::{tr, trf};

slint::slint! {
    import { Button, ProgressIndicator } from "std-widgets.slint";

    export component MessageDialog inherits Window {
        in property <string> window-title;
        in property <string> text;
        in property <string> glyph;
        in property <bool> question;
        in property <string> yes-text;
        in property <string> no-text;
        in property <string> ok-text;
        callback answered(bool);
        title: root.window-title;
        // Fixed widths: slint sizes a wrapped Text by its width, so the window gets the full height.
        width: 480px;
        VerticalLayout {
            padding: 20px;
            spacing: 16px;
            HorizontalLayout {
                spacing: 16px;
                Text { text: root.glyph; font-size: 28px; vertical-alignment: top; width: 32px; }
                Text { text: root.text; wrap: word-wrap; vertical-alignment: top; width: 392px; }
            }
            HorizontalLayout {
                alignment: end;
                spacing: 8px;
                if root.question: Button { text: root.yes-text; primary: true; clicked => { root.answered(true); } }
                if root.question: Button { text: root.no-text; clicked => { root.answered(false); } }
                if !root.question: Button { text: root.ok-text; primary: true; clicked => { root.answered(true); } }
            }
        }
    }

    export component SwitchWindow inherits Window {
        in property <string> heading;
        in property <string> info;
        in property <string> stage;
        in property <string> elapsed;
        in property <bool> done;
        in property <string> close-text;
        in property <image> logo;
        callback close-clicked();
        title: "Asus GPU Tray";
        width: 460px;
        always-on-top: true;
        HorizontalLayout {
            padding-left: 20px;
            padding-right: 20px;
            padding-top: 20px;
            padding-bottom: 16px;
            spacing: 16px;
            VerticalLayout {
                alignment: start;
                Image { source: root.logo; width: 48px; height: 48px; }
            }
            VerticalLayout {
                spacing: 8px;
                Text { text: root.heading; font-size: 18px; font-weight: 700; wrap: word-wrap; width: 356px; }
                Text { text: root.info; wrap: word-wrap; width: 356px; }
                if !root.done: Text { text: root.stage; }
                ProgressIndicator { indeterminate: !root.done; progress: root.done ? 1 : 0; }
                Text { text: root.elapsed; color: #888888; }
                HorizontalLayout {
                    alignment: end;
                    if root.done: Button { text: root.close-text; clicked => { root.close-clicked(); } }
                }
            }
        }
    }
}

thread_local! {
    static OPEN: Cell<u32> = const { Cell::new(0) };
}

/// A dialog is on screen (the tray then holds back its own questions, like Qt's modal check).
pub fn any_open() -> bool {
    OPEN.with(|o| o.get() > 0)
}

#[derive(Default)]
struct Slot {
    value: Option<bool>,
    waker: Option<Waker>,
}

/// Resolves once the dialog is answered.
struct Answer(Rc<RefCell<Slot>>);

impl Future for Answer {
    type Output = bool;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
        let mut slot = self.0.borrow_mut();
        match slot.value {
            Some(v) => Poll::Ready(v),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

fn answer(slot: &Rc<RefCell<Slot>>, value: bool) {
    let mut s = slot.borrow_mut();
    if s.value.is_none() {
        s.value = Some(value);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }
}

#[derive(Clone, Copy)]
pub enum Kind {
    Warning,
    Critical,
    Question,
}

async fn show(kind: Kind, title: &str, text: &str) -> bool {
    let Ok(dialog) = MessageDialog::new() else { return false };
    let glyph = match kind {
        Kind::Warning => "⚠",
        Kind::Critical => "⛔",
        Kind::Question => "?",
    };
    dialog.set_window_title(title.into());
    dialog.set_text(text.into());
    dialog.set_glyph(glyph.into());
    dialog.set_question(matches!(kind, Kind::Question));
    dialog.set_yes_text(tr("Yes").into());
    dialog.set_no_text(tr("No").into());
    dialog.set_ok_text(tr("OK").into());
    let slot = Rc::new(RefCell::new(Slot::default()));
    let s = slot.clone();
    dialog.on_answered(move |v| answer(&s, v));
    let s = slot.clone();
    dialog.window().on_close_requested(move || {
        answer(&s, false);
        slint::CloseRequestResponse::HideWindow
    });
    OPEN.with(|o| o.set(o.get() + 1));
    let _ = dialog.show();
    let value = Answer(slot).await;
    let _ = dialog.hide();
    OPEN.with(|o| o.set(o.get() - 1));
    value
}

/// Plain-text message box with an OK button.
pub async fn message(kind: Kind, title: &str, text: &str) {
    show(kind, title, text).await;
}

/// Yes/No question; true for Yes. Closing the window counts as No.
pub async fn ask(title: &str, text: &str) -> bool {
    show(Kind::Question, title, text).await
}

pub const XG: &str = "XG Mobile";
/// GPU role shown in the progress window; compared untranslated, shown with tr().
pub const BUILTIN: &str = "built-in dGPU";
pub const LIVE_EXPECTED_S: u64 = 40;

pub fn undock_hint() -> String {
    tr("Before you unlock the XG Mobile, switch back to the built-in dGPU in the tray menu. \
        Unlocking it while it is in use needs a reboot.")
}

/// A line of live-progress ("12:00:01 unbind ...") -> what the progress window says, or "".
pub fn switch_stage(step: &str, old: &str, new: &str) -> String {
    let step = step.split_once(' ').map_or(step, |(_, rest)| rest);
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| step.starts_with(p));
    if starts(&["Live switch to", "Nobody holds"]) {
        tr("Preparing…")
    } else if starts(&["unbind", "secondary bus reset", "remove"]) {
        trf("Disconnecting the {gpu}…", &[("gpu", &tr(old))])
    } else if starts(&["link down/up", "egpu_enable"]) {
        tr("Switching the graphics lanes in the firmware…")
    } else if starts(&["enable link", "link on", "Rescanning"]) {
        trf("Connecting the {gpu}…", &[("gpu", &tr(new))])
    } else {
        String::new()
    }
}

fn clock(d: Duration) -> String {
    let t = d.as_secs();
    format!("{}:{:02}", t / 60, t % 60)
}

/// Shown while a live switch runs, like the "Switching in progress" window of Armoury Crate. It
/// cannot be closed until the switch is over.
pub struct SwitchWin {
    win: SwitchWindow,
    done: Rc<Cell<bool>>,
    started: Instant,
    old: String,
    new: String,
}

impl SwitchWin {
    pub fn show(target: &str, old: &str, new: &str) -> Option<Self> {
        let win = SwitchWindow::new().ok()?;
        let done = Rc::new(Cell::new(false));
        win.set_heading(trf("Switching to {target}…", &[("target", target)]).into());
        win.set_info(
            tr("Switching in progress, please wait. Do not disconnect the XG Mobile, close the lid or \
                shut down the computer.")
            .into(),
        );
        win.set_stage(tr("Preparing…").into());
        win.set_close_text(tr("Close").into());
        win.set_logo(super::icon::app_logo(48));
        let weak = win.as_weak();
        win.on_close_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
        let d = done.clone();
        // Alt+F4 while the firmware is switching changes nothing - keep it visible.
        win.window().on_close_requested(move || {
            if d.get() {
                slint::CloseRequestResponse::HideWindow
            } else {
                slint::CloseRequestResponse::KeepWindowShown
            }
        });
        let this = SwitchWin { win, done, started: Instant::now(), old: old.into(), new: new.into() };
        this.update_progress("");
        let _ = this.win.show();
        Some(this)
    }

    pub fn update_progress(&self, step: &str) {
        let stage = switch_stage(step, &self.old, &self.new);
        if !stage.is_empty() {
            self.win.set_stage(stage.into());
        }
        let elapsed = trf(
            "{time} · usually about {seconds} seconds",
            &[("time", &clock(self.started.elapsed())), ("seconds", &LIVE_EXPECTED_S.to_string())],
        );
        self.win.set_elapsed(elapsed.into());
    }

    /// Failure: close right away, a dialog follows. Success: say so, close after 10 s.
    pub fn finish(&self, ok: bool) {
        self.done.set(true);
        if !ok {
            let _ = self.win.hide();
            return;
        }
        self.win.set_heading(trf("Switched to the {gpu}", &[("gpu", &tr(&self.new))]).into());
        let info = if self.new != XG { tr("You can disconnect the XG Mobile now.") } else { undock_hint() };
        self.win.set_info(info.into());
        self.win.set_elapsed(trf("Took {time}", &[("time", &clock(self.started.elapsed()))]).into());
        self.win.set_done(true);
        let weak = self.win.as_weak();
        slint::Timer::single_shot(Duration::from_secs(10), move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages() {
        assert_eq!(switch_stage("12:00:01 unbind 0000:01:00.0", BUILTIN, XG), "Disconnecting the built-in dGPU…");
        assert_eq!(
            switch_stage("12:00:05 egpu_enable=1", BUILTIN, XG),
            "Switching the graphics lanes in the firmware…"
        );
        assert_eq!(switch_stage("12:00:09 Rescanning PCI", BUILTIN, XG), "Connecting the XG Mobile…");
        assert_eq!(switch_stage("12:00:00 Live switch to AsusEgpu", BUILTIN, XG), "Preparing…");
        assert_eq!(switch_stage("12:00:10 something else", BUILTIN, XG), "");
        assert_eq!(clock(Duration::from_secs(75)), "1:15");
    }
}

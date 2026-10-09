//! The tray icon (phase 2): ksni for the icon and menu, slint for the windows and the event loop.

mod app;
mod dialogs;
mod events;
mod icon;
mod lock;
pub mod menu;
mod notify;
mod settings;

use std::path::PathBuf;
use std::process::ExitCode;

use ksni::blocking::TrayMethods;

use crate::gpu::i18n::{self, tr, trf};
use crate::gpu::paths::Paths;

/// Next to the binary when installed (/usr/local/lib/asus-gpu-tray/locale).
fn locale_dir() -> PathBuf {
    let installed = PathBuf::from("/usr/local/lib/asus-gpu-tray/locale");
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("locale")))
        .filter(|d| d.is_dir())
        .unwrap_or(installed)
}

pub fn run() -> ExitCode {
    i18n::init(&locale_dir());
    let _lock = match lock::single_instance_lock() {
        Ok(Some(file)) => file,
        Ok(None) => {
            notify::Notifier::new().show(
                &trf("{app} is already running", &[("app", "Asus GPU Tray")]),
                &tr("The icon is in the system tray."),
                -1,
                false,
            );
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("Cannot create the single-instance lock: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Software rendering: the windows must never open a GPU device node (see dialogs.rs).
    if let Err(e) = slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select()
    {
        eprintln!("Cannot start the UI: {e}");
        return ExitCode::FAILURE;
    }
    let model =
        menu::TrayModel { items: Vec::new(), icon: Vec::new(), tooltip: String::new(), dispatch: app::dispatch };
    // Keeps running without a tray host and shows up when one appears (the panel may come up late).
    let tray = match model.assume_sni_available(true).spawn() {
        Ok(handle) => handle,
        Err(e) => {
            eprintln!("Cannot create the tray icon: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _app = app::App::install(Paths::default(), tray);
    let poll = slint::Timer::default();
    poll.start(slint::TimerMode::Repeated, app::POLL, app::App::tick);
    events::kernel_log(app::kernel_line);
    events::cardwire_signals(app::changed);
    events::udev_pci(app::changed);
    app::App::tick();
    match slint::run_event_loop_until_quit() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Shows one dialog with long sample texts, to check the layout (hidden `preview-dialogs <which>`):
/// ask, warning, switch, switched.
pub fn preview_dialogs(which: &str) -> ExitCode {
    i18n::init(&locale_dir());
    if slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select().is_err() {
        return ExitCode::FAILURE;
    }
    let which = which.to_string();
    let _ = slint::spawn_local(async move {
        match which.as_str() {
            "ask" => {
                let text = tr("Switch to the built-in dGPU now, so the XG Mobile can be unplugged?")
                    + "\n\n"
                    + &tr("These apps use the XG Mobile and will be closed without asking again; unsaved work \
                           in them is lost:")
                    + "\n\n• firefox (PID 1234, 1240)\n• steam (PID 2000)\n\n"
                    + &tr("ROG Control Center is closed and started again, and the GPU services are stopped \
                           during the switch. It takes about 35 seconds. Keep the XG Mobile locked until it is done.");
                dialogs::ask(&tr("Undock now"), &text).await;
            }
            "warning" => {
                dialogs::message(dialogs::Kind::Warning, "XG Mobile", &tr("XG Mobile is not connected and locked."))
                    .await
            }
            "switch" | "switched" => {
                let win = dialogs::SwitchWin::show("XG Mobile (RTX 3070)", dialogs::BUILTIN, dialogs::XG);
                if let Some(w) = &win {
                    w.update_progress("12:00:05 egpu_enable=1");
                    if which == "switched" {
                        w.finish(true);
                    }
                }
                std::future::pending::<()>().await; // until the process is stopped
            }
            _ => {}
        }
        let _ = slint::quit_event_loop();
    });
    let _ = slint::run_event_loop_until_quit();
    ExitCode::SUCCESS
}

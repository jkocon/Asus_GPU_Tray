use std::process::ExitCode;

use clap::{Parser, Subcommand};

use asus_gpu_tray::gpu::cardwire_dbus::DbusCardwire;
use asus_gpu_tray::gpu::cmd::System;
use asus_gpu_tray::gpu::labels::dump;
use asus_gpu_tray::gpu::paths::Paths;
use asus_gpu_tray::gpu::procs::{card_holders, describe_procs, is_root, nvidia_nodes};
use asus_gpu_tray::gpu::state::{read_state, CwRepair};

/// Shows which GPU is rendering and switches GPU modes. Without a subcommand it runs the tray icon.
#[derive(Parser)]
#[command(name = "asus-gpu-tray", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the detected state and exit
    Dump,
    /// List the processes that hold the NVIDIA card; exit status 1 when there are any
    Holders,
    /// Show a dialog with sample texts (layout check): ask, warning, switch, switched
    #[command(hide = true)]
    PreviewDialogs { which: String },
}

fn main() -> ExitCode {
    let paths = Paths::default();
    match Cli::parse().command {
        Some(Command::Dump) => {
            print!(
                "{}",
                dump(&read_state(&paths, &DbusCardwire::connect(), &System, &mut CwRepair::default()), &paths)
            );
            ExitCode::SUCCESS
        }
        Some(Command::Holders) => {
            // As root this sees every process; the undock experiment needs nobody holding the card.
            let procs = card_holders(&nvidia_nodes(&paths), is_root());
            if procs.is_empty() {
                println!("Nobody holds the NVIDIA card");
                ExitCode::SUCCESS
            } else {
                println!("{}", describe_procs(&procs));
                ExitCode::FAILURE
            }
        }
        Some(Command::PreviewDialogs { which }) => asus_gpu_tray::tray::preview_dialogs(&which),
        None => asus_gpu_tray::tray::run(),
    }
}

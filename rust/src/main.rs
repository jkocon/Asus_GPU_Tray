use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
}

fn main() -> ExitCode {
    let paths = Paths::default();
    match Cli::parse().command {
        Some(Command::Dump) => {
            print!("{}", dump(&read_state(&paths, &System, &mut CwRepair::default()), &paths));
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
        None => {
            eprintln!("The tray is not ported yet (phase 2); use asus_gpu_tray.py.");
            ExitCode::from(2)
        }
    }
}

//! Command-line entry point for notetaker.

mod cmd_probe;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "notetaker", version, about = "AI notebook for online meetings")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Diagnose microphone and system audio capture on Windows.
    Probe(cmd_probe::Args),
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Probe(args) => cmd_probe::run(args),
    }
}

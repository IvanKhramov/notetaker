//! Command-line entry point for notetaker.

mod cmd_enhance;
mod cmd_probe;
mod cmd_transcribe;

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
    /// Expand user notes using a transcript, template and configured LLM.
    Enhance(cmd_enhance::Args),
    /// Transcribe a mono WAV using the isolated local ASR worker.
    Transcribe(cmd_transcribe::Args),
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Probe(args) => cmd_probe::run(args),
        Command::Enhance(args) => cmd_enhance::run(args),
        Command::Transcribe(args) => cmd_transcribe::run(args),
    }
}

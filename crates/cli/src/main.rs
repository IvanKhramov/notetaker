//! Command-line entry point for notetaker.

mod cmd_enhance;
mod cmd_probe;
mod cmd_record;
mod cmd_transcribe;
mod cmd_watch;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "notetaker", version, about = "AI notebook for online meetings")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watch microphone usage and print call hints on Windows.
    Watch(cmd_watch::Args),
    /// Diagnose microphone and system audio capture on Windows.
    Probe(cmd_probe::Args),
    /// Expand user notes using a transcript, template and configured LLM.
    Enhance(cmd_enhance::Args),
    /// Record a meeting and transcribe both audio tracks locally.
    Record(cmd_record::Args),
    /// Transcribe a mono WAV using the isolated local ASR worker.
    Transcribe(cmd_transcribe::Args),
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Watch(args) => cmd_watch::run(args),
        Command::Probe(args) => cmd_probe::run(args),
        Command::Enhance(args) => cmd_enhance::run(args),
        Command::Record(args) => cmd_record::run(args),
        Command::Transcribe(args) => cmd_transcribe::run(args),
    }
}

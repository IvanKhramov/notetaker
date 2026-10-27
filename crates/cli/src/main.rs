//! Command-line entry point for notetaker.

mod cmd_enhance;
mod cmd_export;
mod cmd_import;
mod cmd_mcp;
mod cmd_probe;
mod cmd_record;
mod cmd_self_update;
mod cmd_tasks;
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
    /// List action items across meetings.
    Tasks(cmd_tasks::Args),
    #[command(hide = true)]
    SelfUpdateApply(cmd_self_update::Args),
    /// Read-only MCP server over stdio.
    Mcp(cmd_mcp::Options),
    /// Export a transcript or speaker timing log to stdout.
    Export(cmd_export::Args),
    /// Import audio or video and create a meeting with a final transcript.
    Import(cmd_import::Args),
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
        Command::Tasks(args) => cmd_tasks::run(args),
        Command::SelfUpdateApply(args) => cmd_self_update::run(args),
        Command::Mcp(args) => cmd_mcp::run(args),
        Command::Export(args) => cmd_export::run(args),
        Command::Import(args) => cmd_import::run(args),
        Command::Watch(args) => cmd_watch::run(args),
        Command::Probe(args) => cmd_probe::run(args),
        Command::Enhance(args) => cmd_enhance::run(args),
        Command::Record(args) => cmd_record::run(args),
        Command::Transcribe(args) => cmd_transcribe::run(args),
    }
}

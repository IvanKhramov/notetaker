//! Command-line entry point for notetaker.

use clap::Parser;

#[derive(Parser)]
#[command(name = "notetaker", version, about = "AI notebook for online meetings")]
struct Args {}

fn main() {
    let _args = Args::parse();
}

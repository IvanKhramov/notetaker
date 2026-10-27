use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result, bail, ensure};
use notetaker_asr_proto::{AudioInput, Provider};

use crate::{recognizer::Recognizer, tuning};

pub fn run(args: &[String]) -> Result<()> {
    let Some(wav) = args.first() else {
        bail!(
            "usage: notetaker-asr bench <wav> [--model auto|gigaam-v3|parakeet-v3] [--provider cpu|directml] [--threads 0..8] [--runs 1..10]"
        );
    };
    let mut model = "gigaam-v3";
    let mut provider = Provider::Cpu;
    let mut requested_threads = 0;
    let mut runs = 1;
    ensure!(
        !args.len().is_multiple_of(2),
        "benchmark options require values"
    );
    for option in args[1..].as_chunks::<2>().0 {
        match option[0].as_str() {
            "--model" => model = &option[1],
            "--provider" => {
                provider = match option[1].as_str() {
                    "cpu" => Provider::Cpu,
                    "directml" => Provider::Directml,
                    _ => bail!("provider must be cpu or directml"),
                }
            }
            "--threads" => requested_threads = option[1].parse().context("invalid threads")?,
            "--runs" => runs = option[1].parse::<u32>().context("invalid runs")?,
            other => bail!("unknown benchmark option: {other}"),
        }
    }
    ensure!((1..=10).contains(&runs), "runs must be 1..10");
    let threads = tuning::num_threads(requested_threads)?;
    let path = PathBuf::from(wav).canonicalize()?;
    let reader = hound::WavReader::open(&path)?;
    let duration = f64::from(reader.duration()) / f64::from(reader.spec().sample_rate);
    ensure!(duration > 0.0, "empty WAV");
    drop(reader);
    let start = Instant::now();
    let recognizer = Recognizer::load(model, provider, &[], threads)?;
    let load = start.elapsed().as_secs_f64();
    println!(
        "model={model} provider={provider:?} threads={threads} audio_s={duration:.3} load_s={load:.3}"
    );
    for run in 1..=runs {
        let start = Instant::now();
        let segments = recognizer.transcribe(AudioInput::WavPath(path.clone()), 0, |_| Ok(()))?;
        let elapsed = start.elapsed().as_secs_f64();
        println!(
            "run={run} elapsed_s={elapsed:.3} RTF={:.4} segments={}",
            elapsed / duration,
            segments.len()
        );
        for segment in segments {
            println!(
                "[{}-{}] lang={} {}",
                segment.start_ms,
                segment.end_ms,
                segment.lang.as_deref().unwrap_or("unknown"),
                segment.text
            );
        }
    }
    Ok(())
}

//! Isolated speech recognition process. stdout is exclusively JSON lines.
mod bench;
mod diarization;
mod models;
mod recognizer;
mod tuning;

use anyhow::{Context, Result, bail};
use notetaker_asr_proto::{
    PROTOCOL_VERSION, Provider, Request, Response, read_message, write_message,
};
use recognizer::Recognizer;
use std::io::{self, BufRead, Write};

fn main() {
    if let Err(error) = run() {
        eprintln!("notetaker-asr: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => serve(&mut io::stdin().lock(), &mut io::stdout().lock()),
        [command, id] if command == "download" => models::download(id),
        [command, id] if command == "check-model" => models::check(id),
        [command, rest @ ..] if command == "bench" => bench::run(rest),
        [command, provider] if command == "check-provider" => {
            let provider = match provider.as_str() {
                "cpu" => Provider::Cpu,
                "directml" => Provider::Directml,
                _ => bail!("provider must be cpu or directml"),
            };
            println!("{}", recognizer::provider_name(provider)?);
            Ok(())
        }
        [flag] if flag == "--help" || flag == "-h" => {
            eprintln!(
                "Usage: notetaker-asr [download <auto|gigaam-v3|parakeet-v3|whisper-tiny|silero-vad|diarization> | check-model <model_id> | check-provider <cpu|directml>]\nbench <wav> [--model auto|gigaam-v3|parakeet-v3] [--provider cpu|directml] [--threads 0..8] [--runs 1..10]\nWithout arguments: JSON-lines worker on stdin/stdout."
            );
            Ok(())
        }
        _ => bail!("usage: notetaker-asr [download <model_id> | check-model <model_id>]"),
    }
}

fn serve(input: &mut impl BufRead, output: &mut impl Write) -> Result<()> {
    write_message(
        output,
        &Response::Ready {
            protocol_version: PROTOCOL_VERSION,
        },
    )?;
    let mut recognizer = None;
    let mut threads = tuning::num_threads(0)?;
    loop {
        let request: Request = match read_message(input) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_message(
                    output,
                    &Response::Error {
                        id: None,
                        message: error.to_string(),
                    },
                )?;
                // Invalid JSON consumed one line; oversized/truncated frames cannot recover.
                if error.kind() != io::ErrorKind::InvalidData
                    || error.to_string() == "protocol message too large"
                {
                    return Ok(());
                }
                continue;
            }
        };
        let id = request.id().map(str::to_owned);
        let result: Result<Response> = match request {
            Request::Shutdown => return Ok(()),
            Request::LoadModel {
                model_id,
                provider,
                hotwords,
                num_threads,
            } => {
                // Free the old model first: two recognizers can exceed the memory budget.
                recognizer = None;
                tuning::num_threads(num_threads)
                    .and_then(|n| {
                        threads = n;
                        Recognizer::load(&model_id, provider, &hotwords, threads)
                    })
                    .map(|loaded| {
                        recognizer = Some(loaded);
                        Response::ModelLoaded {
                            model_id,
                            provider: match provider {
                                Provider::Auto => Provider::Cpu,
                                p => p,
                            },
                        }
                    })
            }
            Request::Diarize {
                id,
                wav_path,
                channel: _,
                speakers,
                embeddings,
            } => {
                recognizer = None;
                diarization::diarize(&wav_path, speakers, embeddings, threads)
                    .map(|result| Response::Diarized { id, result })
            }
            Request::Embed {
                id,
                wav_path,
                ranges,
            } => {
                recognizer = None;
                diarization::embed(&wav_path, &ranges, threads)
                    .map(|embedding| Response::Embedded { id, embedding })
            }
            Request::Transcribe {
                id,
                input,
                channel: _,
                offset_ms,
            } => recognizer
                .as_ref()
                .context("load a model before transcribe")
                .and_then(|r| {
                    r.transcribe(input, offset_ms, |fraction| {
                        write_message(
                            output,
                            &Response::Progress {
                                id: id.clone(),
                                fraction,
                            },
                        )?;
                        Ok(())
                    })
                })
                .map(|segments| Response::Segments { id, segments }),
        };
        let response = result.unwrap_or_else(|error| Response::Error {
            id,
            message: format!("{error:#}"),
        });
        write_message(output, &response)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_input_and_missing_model_are_recoverable() {
        let input = b"bad json\n{\"type\":\"transcribe\",\"id\":\"t\",\"input\":{\"type\":\"pcm\",\"data\":{\"samples_f32_16k_mono\":\"\"}},\"channel\":\"mic\",\"offset_ms\":0}\n{\"type\":\"shutdown\"}\n";
        let mut output = Vec::new();
        serve(&mut input.as_slice(), &mut output).unwrap();
        let responses: Vec<Response> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert!(matches!(responses[0], Response::Ready { .. }));
        assert!(matches!(&responses[1], Response::Error { id: None, .. }));
        assert!(matches!(&responses[2], Response::Error { id: Some(id), .. } if id == "t"));
    }
}

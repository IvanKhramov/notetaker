//! Versioned JSON-lines protocol for the isolated ASR process.
//!
//! Each message has a `type` tag. PCM is base64 of little-endian IEEE f32,
//! normalized to [-1, 1], mono at 16 kHz. Timestamps include `offset_ms`.
//! A client processes one request at a time; channels are caller-owned labels.

mod client;
pub use client::{AsrClient, CancellationHandle, ClientError};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u32 = 1;
/// Bounds allocations for either side of the process boundary (about 12 min PCM).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Cpu,
    Cuda,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AudioInput {
    WavPath(PathBuf),
    Pcm { samples_f32_16k_mono: String },
}

impl AudioInput {
    pub fn pcm(samples: &[f32]) -> Self {
        let bytes: Vec<_> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self::Pcm {
            samples_f32_16k_mono: STANDARD.encode(bytes),
        }
    }

    pub fn decode_pcm(encoded: &str) -> Result<Vec<f32>, String> {
        let bytes = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
        let (values, remainder) = bytes.as_chunks::<4>();
        if !remainder.is_empty() {
            return Err("PCM byte count must be divisible by four".into());
        }
        let samples: Vec<_> = values.iter().map(|b| f32::from_le_bytes(*b)).collect();
        if samples.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
            return Err("PCM must contain finite samples in [-1, 1]".into());
        }
        Ok(samples)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    LoadModel {
        model_id: String,
        #[serde(default)]
        provider: Provider,
    },
    Transcribe {
        id: String,
        input: AudioInput,
        channel: String,
        offset_ms: u64,
    },
    Shutdown,
}

impl Request {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Transcribe { id, .. } => Some(id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ready {
        protocol_version: u32,
    },
    ModelLoaded {
        model_id: String,
        provider: Provider,
    },
    Segments {
        id: String,
        segments: Vec<Segment>,
    },
    /// Samples processed, from 0 to 1; emitted while transcribing speech chunks.
    Progress {
        id: String,
        fraction: f32,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
    },
}

/// Read exactly one bounded line. EOF between messages is distinct from bad JSON.
pub fn read_message<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    let count =
        std::io::Read::take(reader, (MAX_MESSAGE_BYTES + 1) as u64).read_until(b'\n', &mut line)?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "protocol message too large",
        ));
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete protocol line",
        ));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() >= MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "protocol message too large",
        ));
    }
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_roundtrips() {
        let requests = [
            Request::LoadModel {
                model_id: "gigaam-v3".into(),
                provider: Provider::Auto,
            },
            Request::Transcribe {
                id: "a".into(),
                input: AudioInput::WavPath("файл.wav".into()),
                channel: "mic".into(),
                offset_ms: 123,
            },
            Request::Transcribe {
                id: "b".into(),
                input: AudioInput::pcm(&[0.5, -0.25]),
                channel: "system".into(),
                offset_ms: 0,
            },
            Request::Shutdown,
        ];
        for request in requests {
            let mut bytes = Vec::new();
            write_message(&mut bytes, &request).unwrap();
            assert_eq!(
                read_message::<Request>(&mut bytes.as_slice()).unwrap(),
                Some(request)
            );
        }
        let responses = [
            Response::Ready {
                protocol_version: 1,
            },
            Response::ModelLoaded {
                model_id: "gigaam-v3".into(),
                provider: Provider::Cpu,
            },
            Response::Progress {
                id: "a".into(),
                fraction: 0.5,
            },
            Response::Segments {
                id: "a".into(),
                segments: vec![Segment {
                    start_ms: 123,
                    end_ms: 456,
                    text: "Привет!".into(),
                }],
            },
            Response::Error {
                id: None,
                message: "bad model".into(),
            },
            Response::Error {
                id: Some("a".into()),
                message: "bad audio".into(),
            },
        ];
        for response in responses {
            let mut bytes = Vec::new();
            write_message(&mut bytes, &response).unwrap();
            assert_eq!(
                read_message::<Response>(&mut bytes.as_slice()).unwrap(),
                Some(response)
            );
        }
        assert_eq!(
            serde_json::to_string(&Request::Shutdown).unwrap(),
            r#"{"type":"shutdown"}"#
        );
    }
    #[test]
    fn pcm_validation_and_framing() {
        let AudioInput::Pcm {
            samples_f32_16k_mono,
        } = AudioInput::pcm(&[0.5, -1.0])
        else {
            unreachable!()
        };
        assert_eq!(
            AudioInput::decode_pcm(&samples_f32_16k_mono).unwrap(),
            [0.5, -1.0]
        );
        for data in [
            "?".into(),
            STANDARD.encode([0u8]),
            STANDARD.encode(f32::NAN.to_le_bytes()),
            STANDARD.encode(2f32.to_le_bytes()),
        ] {
            assert!(AudioInput::decode_pcm(&data).is_err());
        }
        assert!(read_message::<Request>(&mut b"{}".as_slice()).is_err());
        assert!(read_message::<Request>(&mut b"invalid\n".as_slice()).is_err());
        assert!(
            read_message::<Request>(&mut b"".as_slice())
                .unwrap()
                .is_none()
        );
    }
}

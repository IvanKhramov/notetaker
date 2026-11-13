//! Recording continues independently of the single, sequential ASR worker.
mod asr;
mod diarization;
pub use diarization::{DiarizationConfig, rediarize};
mod audio;
mod decode;
mod echo;
mod import;
mod recover;
pub use echo::suppress_echo;
mod session;

pub use asr::{Asr, AsrConfig, WorkerAsr};
pub use audio::ChunkConfig;
pub use session::{PipelineEvent, RecordingConfig, RecordingSession, StopHandle};

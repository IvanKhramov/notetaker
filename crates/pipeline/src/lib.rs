//! Recording continues independently of the single, sequential ASR worker.
mod asr;
mod diarization;
mod voices;
pub use diarization::{DiarizationConfig, rediarize};
pub use voices::{
    HistoricalEnrollment, LearningSpaceError, cleanup_learning_temp, learn_recording,
    named_voice_embedding, remember_named_voices,
};
mod audio;
mod decode;
mod echo;
mod import;
mod recover;
pub use echo::suppress_echo;
pub mod live_speakers;
mod session;

pub use asr::{Asr, AsrConfig, WorkerAsr};
pub use audio::ChunkConfig;
pub use session::{PipelineEvent, RecordingConfig, RecordingSession, StopHandle};

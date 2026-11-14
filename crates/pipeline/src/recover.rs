//! Runs the final pass again from the saved audio of an existing meeting. This is
//! the way back after a crash, a power loss or a failed final pass: the opus tracks
//! are written continuously, so they outlive the process that recorded them.
use std::{fs, sync::mpsc::Sender, time::Duration};

use anyhow::{Context, Result, ensure};
use chrono::{TimeDelta, Utc};
use notetaker_asr_proto::AudioInput;
use notetaker_capture::Channel;
use notetaker_core::{Meeting, Source};

use crate::{Asr, PipelineEvent, RecordingConfig, StopHandle, decode, session::convert};

pub(crate) fn run(
    meeting: &Meeting,
    mut asr: impl Asr,
    config: RecordingConfig,
    stop: StopHandle,
    events: &Sender<PipelineEvent>,
) -> Result<()> {
    let work = tempfile::tempdir_in(meeting.path().join("audio"))?;
    let tracks = [
        (meeting.mic_audio_path(), Channel::Mic, "mic.wav"),
        (meeting.system_audio_path(), Channel::System, "system.wav"),
    ];
    let total = 4;
    let mut completed = 0;
    let progress = |completed: usize| {
        let _ = events.send(PipelineEvent::FinalPassProgress { completed, total });
    };
    progress(0);
    let mut wavs = Vec::new();
    let mut longest = 0;
    for (audio, channel, name) in tracks {
        if audio.is_file() {
            let wav = work.path().join(name);
            let count = decode::decode_recording(&audio, &wav, |_| {
                ensure!(!stop.is_stopped(), "Распознавание отменено");
                Ok(())
            })?;
            longest = longest.max(count);
            wavs.push((wav, channel));
        }
        completed += 1;
        progress(completed);
    }
    ensure!(!wavs.is_empty(), "Аудио этой встречи не сохранено");
    // The default request deadline is five minutes, too short for hour-long files.
    asr.set_timeout(
        Duration::from_secs(300)
            .saturating_add(Duration::from_millis(longest / 16).saturating_mul(4)),
    );
    asr.start()?;
    if let Some(notice) = asr.take_notice() {
        let _ = events.send(PipelineEvent::Warning(notice));
    }
    let mut segments = Vec::new();
    for (wav, channel) in &wavs {
        ensure!(!stop.is_stopped(), "Распознавание отменено");
        segments.extend(convert(
            asr.transcribe(AudioInput::WavPath(wav.clone()), *channel, 0)?,
            *channel,
            Source::Final,
        ));
        completed += 1;
        progress(completed);
    }
    ensure!(!stop.is_stopped(), "Распознавание отменено");
    meeting.update_transcript(|transcript| {
        transcript.segments =
            crate::session::replace_final(std::mem::take(&mut transcript.segments), segments);
        crate::echo::suppress_echo(&mut transcript.segments);
    })?;
    let wav_of = |wanted: Channel| {
        wavs.iter()
            .find(|(_, channel)| *channel == wanted)
            .map(|(wav, _)| wav.as_path())
    };
    if let Some(system) = wav_of(Channel::System) {
        crate::diarization::final_pass(
            meeting,
            &mut asr,
            system,
            wav_of(Channel::Mic),
            &config.diarization,
            events,
        );
    }
    let mut meta = meeting.read_meta()?;
    if meta.ended_at.is_none() {
        // An interrupted recording never got its end; the audio length is the best estimate.
        meta.ended_at = Some(
            meta.started_at
                .checked_add_signed(TimeDelta::milliseconds((longest / 16).try_into()?))
                .context("Слишком длинная запись")?,
        );
        meta.audio_expires_at = config
            .retention
            .map(|duration| -> Result<_> {
                Utc::now()
                    .checked_add_signed(TimeDelta::from_std(duration)?)
                    .context("Слишком большой срок хранения")
            })
            .transpose()?;
        meeting.update_meta(|current| {
            current.ended_at = meta.ended_at;
            current.audio_expires_at = meta.audio_expires_at;
        })?;
    }
    // Leftovers of the interrupted recording; the transcript no longer needs them.
    let _ = fs::remove_dir_all(meeting.path().join("audio/.work"));
    progress(total);
    let _ = events.send(PipelineEvent::Finished {
        meeting: meeting.clone(),
    });
    Ok(())
}

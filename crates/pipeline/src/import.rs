use std::{fs, path::Path, sync::mpsc::Sender, time::Duration};

use anyhow::{Context, Result, ensure};
use chrono::{TimeDelta, Utc};
use notetaker_asr_proto::AudioInput;
use notetaker_capture::{Channel, TrackWriter};
use notetaker_core::{Meeting, Source};

use crate::{Asr, PipelineEvent, RecordingConfig, StopHandle, decode, session::convert};

pub(crate) fn run(
    meeting: &Meeting,
    file: &Path,
    mut asr: impl Asr,
    config: RecordingConfig,
    stop: StopHandle,
    events: &Sender<PipelineEvent>,
) -> Result<()> {
    let work = tempfile::tempdir_in(meeting.path().join("audio"))?;
    let wav = work.path().join("import.wav");
    let mut last_progress = usize::MAX;
    let mut progress = |completed| {
        if completed == last_progress {
            return;
        }
        last_progress = completed;
        let _ = events.send(PipelineEvent::FinalPassProgress {
            completed,
            total: 200,
        });
    };
    progress(0);
    let count = decode::decode(file, &wav, |fraction| {
        ensure!(!stop.is_stopped(), "Импорт отменён");
        progress((fraction * 100.0) as usize);
        Ok(())
    })?;
    let mut reader = hound::WavReader::open(&wav)?;
    let mut opus = TrackWriter::create(meeting.system_audio_path())?;
    let mut samples = reader.samples::<f32>();
    loop {
        ensure!(!stop.is_stopped(), "Импорт отменён");
        let block: Vec<_> = samples.by_ref().take(8192).collect::<Result<_, _>>()?;
        if block.is_empty() {
            break;
        }
        opus.write_samples(&block)?;
    }
    opus.finish()?;
    drop(reader);
    let mut meta = meeting.read_meta()?;
    meta.ended_at = Some(
        meta.started_at
            .checked_add_signed(TimeDelta::milliseconds((count / 16).try_into()?))
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
    meeting.write_meta(&meta)?;
    progress(100);
    // The default request deadline is five minutes, too short for hour-long
    // files. Keep a finite total budget, allowing slower-than-realtime CPUs.
    asr.set_timeout(
        Duration::from_secs(300)
            .saturating_add(Duration::from_millis(count / 16).saturating_mul(4)),
    );
    asr.start()?;
    if let Some(notice) = asr.take_notice() {
        let _ = events.send(PipelineEvent::Warning(notice));
    }
    let cancel = asr.cancellation_handle();
    let segments = asr.transcribe_with_progress(
        AudioInput::WavPath(wav.clone()),
        Channel::System,
        0,
        &mut |fraction| {
            if stop.is_stopped()
                && let Some(cancel) = &cancel
            {
                cancel.cancel();
            }
            progress(100 + (fraction.clamp(0.0, 1.0) * 100.0) as usize);
        },
    );
    ensure!(!stop.is_stopped(), "Импорт отменён");
    let segments = segments?;
    meeting.update_transcript(|transcript| {
        transcript.segments = crate::session::replace_final(
            std::mem::take(&mut transcript.segments),
            convert(segments, Channel::System, Source::Final),
        );
    })?;
    crate::diarization::final_pass(meeting, &mut asr, &wav, None, &config.diarization, events);
    if config.retention == Some(Duration::ZERO) {
        fs::remove_file(meeting.system_audio_path())?;
    }
    progress(200);
    let _ = events.send(PipelineEvent::Finished {
        meeting: meeting.clone(),
    });
    Ok(())
}

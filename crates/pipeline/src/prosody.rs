//! Measure one frame at a time in the final pass and on retained recordings.
mod yin;

use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{CancellationHandle, SpeakerTurn};
use notetaker_core::{
    Channel, Meeting, Segment, Transcript,
    prosody::{Prosody, SegmentProsody, quantile, thresholds::*},
};

use crate::{PipelineEvent, decode};
use yin::{FRAME_SAMPLES, SAMPLE_RATE};

pub(crate) fn final_pass(
    meeting: &Meeting,
    system: Option<&Path>,
    mic: Option<&Path>,
    system_turns: &[SpeakerTurn],
    events: &Sender<PipelineEvent>,
) {
    let result = (|| -> Result<()> {
        let transcript = meeting.read_transcript()?;
        meeting.write_prosody(&measure(
            transcript.segments.iter(),
            system,
            mic,
            system_turns,
            &|| Ok(()),
        )?)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = events.send(PipelineEvent::Warning(format!("Просодия: {error:#}")));
    }
}

#[derive(Clone, Copy)]
pub enum ProsodyStage {
    Decode,
    Diarize,
    Measure,
}

fn retained_tracks(meeting: &Meeting, transcript: &Transcript) -> [Option<PathBuf>; 2] {
    [
        (Channel::Mic, meeting.mic_audio_path()),
        (Channel::System, meeting.system_audio_path()),
    ]
    .map(|(channel, path)| {
        (transcript.segments.iter().any(|s| s.channel == channel) && path.is_file()).then_some(path)
    })
}

pub fn has_prosody_audio(meeting: &Meeting, transcript: &Transcript) -> bool {
    retained_tracks(meeting, transcript)
        .iter()
        .any(Option::is_some)
}

/// Decode retained tracks to temporary WAVs and measure prosody without reassigning speakers.
pub fn recompute_prosody(
    meeting: &Meeting,
    progress: impl FnMut(ProsodyStage),
    diarize: impl FnOnce(&Path, Duration) -> Result<Vec<SpeakerTurn>>,
) -> Result<String> {
    let (prosody, transcript) =
        recompute(meeting, &CancellationHandle::default(), progress, diarize)?;
    Ok(prosody.diagnostic_text(&transcript))
}

pub fn recompute_prosody_cancellable(
    meeting: &Meeting,
    cancel: &CancellationHandle,
    diarize: impl FnOnce(&Path, Duration) -> Result<Vec<SpeakerTurn>>,
) -> Result<()> {
    recompute(meeting, cancel, |_| {}, diarize)?;
    Ok(())
}

fn recompute(
    meeting: &Meeting,
    cancel: &CancellationHandle,
    mut progress: impl FnMut(ProsodyStage),
    diarize: impl FnOnce(&Path, Duration) -> Result<Vec<SpeakerTurn>>,
) -> Result<(Prosody, Transcript)> {
    let check = || {
        ensure!(
            !cancel.is_cancelled(),
            "Пересчёт речевых показателей отменён"
        );
        Ok(())
    };
    check()?;
    let transcript = meeting.read_transcript()?;
    let [mic, system] = retained_tracks(meeting, &transcript);
    ensure!(
        mic.is_some() || system.is_some(),
        "Нет сохранённого аудио для реплик этой встречи"
    );
    let work = tempfile::tempdir_in(meeting.path().join("audio"))?;
    progress(ProsodyStage::Decode);
    let decode_track = |input: Option<&Path>, name| -> Result<_> {
        let Some(input) = input else {
            return Ok(None);
        };
        let wav = work.path().join(name);
        let count = decode::decode_recording(input, &wav, |_| check())
            .with_context(|| format!("Чтение {}", input.display()))?;
        Ok(Some((wav, count)))
    };
    let mic = decode_track(mic.as_deref(), "mic.wav")?;
    let system = decode_track(system.as_deref(), "system.wav")?;
    check()?;
    let system_turns = if let Some((path, count)) = &system {
        progress(ProsodyStage::Diarize);
        diarize(path, Duration::from_millis(count / 16))?
    } else {
        Vec::new()
    };
    check()?;
    progress(ProsodyStage::Measure);
    let segments = transcript.segments.iter().filter(|s| match s.channel {
        Channel::Mic => mic.is_some(),
        Channel::System => system.is_some(),
    });
    let prosody = measure(
        segments,
        system.as_ref().map(|(path, _)| path.as_path()),
        mic.as_ref().map(|(path, _)| path.as_path()),
        &system_turns,
        &check,
    )?;
    check()?;
    if !meeting.write_prosody_if(&prosody, || !cancel.is_cancelled())? {
        check()?;
    }
    Ok((prosody, transcript))
}

fn measure<'a>(
    segments: impl Iterator<Item = &'a Segment> + Clone,
    system: Option<&Path>,
    mic: Option<&Path>,
    system_turns: &[SpeakerTurn],
    check: &impl Fn() -> Result<()>,
) -> Result<Prosody> {
    let mut prosody = Prosody::default();
    for (channel, path) in [(Channel::Mic, mic), (Channel::System, system)] {
        check()?;
        let segments: Vec<_> = segments.clone().filter(|s| s.channel == channel).collect();
        if segments.is_empty() {
            continue;
        }
        let path = path.with_context(|| format!("Нет дорожки {channel:?}"))?;
        let mut reader =
            hound::WavReader::open(path).with_context(|| format!("Чтение {}", path.display()))?;
        let spec = reader.spec();
        ensure!(
            spec.sample_rate == SAMPLE_RATE as u32 && spec.channels == 1,
            "Ожидается моно WAV 16 кГц"
        );
        // Diarization sees acoustic overlap even when ASR keeps a single
        // segment for both voices. Transcript speaker edits are independent.
        let mut overlaps = if channel == Channel::System {
            overlap_spans(
                system_turns
                    .iter()
                    .map(|t| (t.start_ms, t.end_ms, t.speaker.as_str())),
            )
        } else {
            Vec::new()
        };
        // Prefix maxima allow a query by start time even for nested spans.
        let mut latest_end = 0;
        for (_, end) in &mut overlaps {
            latest_end = latest_end.max(*end);
            *end = latest_end;
        }
        for s in segments {
            check()?;
            let duration = s.end_ms.saturating_sub(s.start_ms);
            let preceding = overlaps.partition_point(|&(start, _)| start < s.end_ms);
            let mut raw = SegmentProsody {
                same_channel_overlap: duration > 0
                    && preceding > 0
                    && s.start_ms < overlaps[preceding - 1].1,
                chars_per_second: (duration >= MIN_METRIC_MS)
                    .then(|| s.text.trim().chars().count() as f32 * 1000.0 / duration as f32),
                ..Default::default()
            };
            let start = sample_index(s.start_ms, reader.duration());
            let end = sample_index(s.end_ms, reader.duration());
            // Without diarization turns overlap on the system track is unknown,
            // so its mixed audio must not count as one person's voice.
            let overlap_known = channel == Channel::Mic || !system_turns.is_empty();
            if duration >= MIN_METRIC_MS
                && overlap_known
                && !raw.same_channel_overlap
                && end > start
            {
                reader.seek(start)?;
                let count = (end - start) as usize;
                let sound = match spec.sample_format {
                    hound::SampleFormat::Float => {
                        acoustics(reader.samples::<f32>().take(count), check)?
                    }
                    hound::SampleFormat::Int => {
                        let scale = 2.0_f32.powi(i32::from(spec.bits_per_sample) - 1);
                        acoustics(
                            reader
                                .samples::<i32>()
                                .take(count)
                                .map(|s| s.map(|s| s as f32 / scale)),
                            check,
                        )?
                    }
                };
                raw.loudness_dbfs = sound.loudness_dbfs;
                raw.f0_hz = sound.f0_hz;
                raw.f0_iqr_st = sound.f0_iqr_st;
            }
            prosody.segments.insert(s.id, raw);
        }
    }
    Ok(prosody)
}

fn sample_index(ms: u64, max: u32) -> u32 {
    ms.saturating_mul(SAMPLE_RATE as u64 / 1000)
        .min(u64::from(max)) as u32
}

fn overlap_spans<'a>(intervals: impl Iterator<Item = (u64, u64, &'a str)>) -> Vec<(u64, u64)> {
    let mut intervals: Vec<_> = intervals.filter(|(start, end, _)| start < end).collect();
    intervals.sort_unstable();
    let mut spans = Vec::new();
    // Keep the latest end times for two distinct speakers. For each new
    // interval only the latest prior end from a different speaker matters.
    let mut latest = ("", 0);
    let mut second = ("", 0);
    for (start, end, speaker) in intervals {
        let prior_end = if latest.0 == speaker {
            second.1
        } else {
            latest.1
        };
        if prior_end > start {
            spans.push((start, end.min(prior_end)));
        }
        if end > latest.1 {
            if latest.0 != speaker {
                second = latest;
            }
            latest = (speaker, end);
        } else if latest.0 != speaker && end > second.1 {
            second = (speaker, end);
        }
    }
    spans
}

fn acoustics(
    samples: impl Iterator<Item = hound::Result<f32>>,
    check: &impl Fn() -> Result<()>,
) -> Result<SegmentProsody> {
    let mut frame = [0.0; FRAME_SAMPLES];
    let mut filled = 0;
    let mut energy_sum = 0.0_f64;
    let mut speech_frames = 0;
    let mut pitches = Vec::new();
    let min_energy = 10_f32.powf(MIN_SPEECH_DBFS / 10.0) * FRAME_SAMPLES as f32;
    for sample in samples {
        frame[filled] = sample?;
        filled += 1;
        if filled != frame.len() {
            continue;
        }
        check()?;
        filled = 0;
        let energy = frame.iter().map(|x| x * x).sum::<f32>();
        if energy >= min_energy {
            energy_sum += f64::from(energy);
            speech_frames += 1;
            pitches.extend(yin::pitch(&frame));
        }
    }
    let mut raw = SegmentProsody::default();
    if speech_frames > 0 {
        let mean_square = energy_sum / (speech_frames * FRAME_SAMPLES) as f64;
        raw.loudness_dbfs = Some((10.0 * mean_square.log10()) as f32);
    }
    if let Some((median, iqr)) = pitch_stats(pitches) {
        raw.f0_hz = Some(median);
        raw.f0_iqr_st = Some(iqr);
    }
    Ok(raw)
}

fn pitch_stats(mut pitches: Vec<f32>) -> Option<(f32, f32)> {
    if pitches.len() < MIN_VOICED_FRAMES {
        return None;
    }
    pitches.sort_unstable_by(f32::total_cmp);
    let median = quantile(&pitches, 0.5);
    let min = median / MAX_FRAME_F0_RATIO;
    let max = median * MAX_FRAME_F0_RATIO;
    pitches.retain(|&hz| (min..=max).contains(&hz));
    (pitches.len() >= MIN_VOICED_FRAMES).then(|| {
        (
            quantile(&pitches, 0.5),
            12.0 * (quantile(&pitches, 0.75) / quantile(&pitches, 0.25)).log2(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Library, MeetingMeta, Segment, Source, SpeakerId};

    #[test]
    fn loudness_uses_speech_frames_and_tracks_scale() {
        let measure = |gain| {
            acoustics(
                [yin::tests::sine(220.0, gain), [0.0; FRAME_SAMPLES]]
                    .into_iter()
                    .flatten()
                    .map(Ok),
                &|| Ok(()),
            )
            .unwrap()
            .loudness_dbfs
            .unwrap()
        };
        let quiet = measure(0.25);
        assert!((quiet - 20.0 * (0.25 / 2.0_f32.sqrt()).log10()).abs() < 0.1);
        assert!((measure(0.5) - quiet - 6.02).abs() < 0.01);
    }

    #[test]
    fn same_channel_overlap_uses_diarized_intersections() {
        let spans = overlap_spans(
            [
                (0, 1000, "a"),
                (700, 1200, "a"),
                (900, 1100, "b"),
                (1200, 1500, "c"),
            ]
            .into_iter(),
        );
        assert_eq!(spans, [(900, 1100)]);
    }

    #[test]
    fn pitch_statistics_discard_octave_errors_before_calculating_spread() {
        let (median, iqr) = pitch_stats(vec![60.0, 118.0, 120.0, 122.0, 240.0]).unwrap();
        assert_eq!(median, 120.0);
        assert!((iqr - 12.0 * (121.0_f32 / 119.0).log2()).abs() < 0.001);
        assert!(pitch_stats(vec![60.0, 120.0, 240.0]).is_none());
    }

    #[test]
    fn recompute_excludes_mixed_opus_audio_and_does_not_reassign_speakers() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Test", chrono::Utc::now()))
            .unwrap();
        let mut transcript = Transcript::default();
        for (start_ms, end_ms) in [(0, 3000), (3000, 6000), (6000, 8999)] {
            transcript.segments.push(Segment {
                id: uuid::Uuid::now_v7(),
                speaker: SpeakerId("edited-speaker".into()),
                channel: Channel::System,
                start_ms,
                end_ms,
                text: "Текст сегмента".into(),
                source: Source::Final,
                edited: true,
                lang: None,
            });
        }
        meeting.write_transcript(&transcript).unwrap();
        assert!(!has_prosody_audio(&meeting, &transcript));
        std::fs::write(meeting.mic_audio_path(), []).unwrap();
        assert!(!has_prosody_audio(&meeting, &transcript));
        std::fs::remove_file(meeting.mic_audio_path()).unwrap();
        let (events, warnings) = std::sync::mpsc::channel();
        final_pass(&meeting, None, None, &[], &events);
        assert!(matches!(
            warnings.try_recv().unwrap(),
            PipelineEvent::Warning(message) if message.starts_with("Просодия:")
        ));
        assert_eq!(meeting.read_transcript().unwrap(), transcript);
        assert!(meeting.read_prosody().unwrap().is_none());

        let mut audio =
            notetaker_capture::TrackWriter::create(meeting.system_audio_path()).unwrap();
        for _ in 0..9_000 / FRAME_MS {
            audio.write_samples(&yin::tests::sine(120.0, 0.3)).unwrap();
        }
        audio.finish().unwrap();
        assert!(has_prosody_audio(&meeting, &transcript));

        let cancel = CancellationHandle::default();
        let result = recompute_prosody_cancellable(&meeting, &cancel, |_, _| {
            cancel.cancel();
            Ok(Vec::new())
        });
        assert!(result.unwrap_err().to_string().contains("отменён"));
        assert!(meeting.read_prosody().unwrap().is_none());

        recompute_prosody(
            &meeting,
            |_| {},
            |wav, duration| {
                assert_eq!(duration, Duration::from_secs(9));
                let turns = vec![
                    SpeakerTurn {
                        start_ms: 0,
                        end_ms: 9000,
                        speaker: "a".into(),
                    },
                    SpeakerTurn {
                        start_ms: 4500,
                        end_ms: 5500,
                        speaker: "b".into(),
                    },
                ];
                final_pass(&meeting, Some(wav), None, &turns, &events);
                assert!(!meeting.prosody_needs_recompute().unwrap());
                std::fs::write(
                    meeting.path().join("prosody.json"),
                    r#"{"version":1,"segments":{}}"#,
                )
                .unwrap();
                Ok(turns)
            },
        )
        .unwrap();
        assert!(!meeting.prosody_needs_recompute().unwrap());
        assert_eq!(meeting.read_transcript().unwrap(), transcript);
        let prosody = meeting.read_prosody().unwrap().unwrap();
        let clean = &prosody.segments[&transcript.segments[0].id];
        assert!(!clean.same_channel_overlap);
        assert!(clean.loudness_dbfs.is_some());
        let mixed = &prosody.segments[&transcript.segments[1].id];
        assert!(mixed.same_channel_overlap);
        assert!(mixed.loudness_dbfs.is_none() && mixed.f0_hz.is_none());
        assert!(mixed.chars_per_second.is_some());
        let short = &prosody.segments[&transcript.segments[2].id];
        assert!(short.loudness_dbfs.is_none());
        assert!(short.f0_hz.is_none() && short.f0_iqr_st.is_none());
        assert!(short.chars_per_second.is_none());

        // No diarization turns: overlap is unknown, so no system acoustics.
        let mut missing_mic = transcript.segments[0].clone();
        missing_mic.channel = Channel::Mic;
        missing_mic.id = uuid::Uuid::now_v7();
        transcript.segments.push(missing_mic.clone());
        meeting.write_transcript(&transcript).unwrap();
        recompute_prosody(&meeting, |_| {}, |_, _| Ok(Vec::new())).unwrap();
        let prosody = meeting.read_prosody().unwrap().unwrap();
        assert!(!prosody.segments.contains_key(&missing_mic.id));
        for segment in &transcript.segments[..2] {
            let raw = &prosody.segments[&segment.id];
            assert!(raw.loudness_dbfs.is_none() && raw.f0_hz.is_none());
            assert!(raw.chars_per_second.is_some());
        }
    }
}

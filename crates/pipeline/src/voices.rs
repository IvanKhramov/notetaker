//! Recompute fingerprints after external speaker assignments, without rerunning ASR.
use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::{SpeakerTurn, clean_ranges};
use notetaker_core::{Channel, Library, Meeting, Transcript};
use std::collections::{BTreeMap, BTreeSet};

pub fn remember_named_voices(
    meeting: &Meeting,
    transcript: &Transcript,
    mut embed: impl FnMut(&std::path::Path, Vec<notetaker_asr_proto::TimeRange>) -> Result<Vec<f32>>,
) -> Result<usize> {
    ensure!(
        meeting.mic_audio_path().is_file() || meeting.system_audio_path().is_file(),
        "Аудио не хранится: голоса не запомнены"
    );
    let work = tempfile::tempdir()?;
    let mut embeddings = BTreeMap::new();
    for (channel, source) in [
        (Channel::Mic, meeting.mic_audio_path()),
        (Channel::System, meeting.system_audio_path()),
    ] {
        let turns: Vec<_> = transcript
            .segments
            .iter()
            .filter(|s| s.channel == channel)
            .map(|s| SpeakerTurn {
                speaker: s.speaker.0.clone(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
            })
            .collect();
        if turns.is_empty() {
            continue;
        }
        ensure!(
            source.is_file(),
            "Аудио дорожки не хранится: голоса не запомнены"
        );
        let wav = work.path().join(format!("{channel:?}.wav"));
        crate::decode::decode(&source, &wav, |_| Ok(()))?;
        let ids: BTreeSet<_> = turns.iter().map(|turn| turn.speaker.clone()).collect();
        for id in ids {
            if embeddings.contains_key(&id) {
                continue;
            }
            let ranges: Vec<_> = clean_ranges(&turns, &id)
                .into_iter()
                .filter(|range| range.end_ms.saturating_sub(range.start_ms) >= 1000)
                .take(8)
                .collect();
            if ranges.is_empty() {
                continue;
            }
            embeddings.insert(id, embed(&wav, ranges)?);
        }
    }
    let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
    ensure!(
        meeting.read_transcript()? == *transcript,
        "Транскрипт изменился: повторите обучение голосов"
    );
    // Never assign an old centroid to the newly assembled Tolk speaker IDs.
    library.write_meeting_voices(meeting, embeddings.clone())?;
    let mut named = transcript.clone();
    named.speaker_names.retain(|id, _| {
        transcript.ktalk_sources.contains_key(id)
            || id == "me"
            || transcript.self_speaker.as_ref() == Some(id)
    });
    let count = named
        .speaker_names
        .keys()
        .filter(|id| embeddings.contains_key(*id))
        .count();
    library.remember_speakers(meeting, &named)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{MeetingMeta, Segment, Source, SpeakerId};
    fn wav(path: &std::path::Path) {
        let mut file = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..160_000 {
            file.write_sample(0_i16).unwrap();
        }
        file.finalize().unwrap();
    }
    #[test]
    fn reassigned_participants_guest_and_microphone_owner_use_fresh_clean_embeddings() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new(
                "Синтетическая встреча",
                chrono::Utc::now(),
            ))
            .unwrap();
        wav(&meeting.system_audio_path());
        wav(&meeting.mic_audio_path());
        let mut transcript = Transcript::default();
        for (id, channel, start, end, name) in [
            ("ktalk-registered", Channel::System, 0, 4000, "Anna Petrova"),
            ("ktalk-guest", Channel::System, 3000, 7000, "Гость"),
            ("me", Channel::Mic, 0, 4000, "Олег Сидоров"),
        ] {
            transcript.segments.push(Segment {
                id: uuid::Uuid::now_v7(),
                speaker: SpeakerId(id.into()),
                channel,
                start_ms: start,
                end_ms: end,
                text: "Проверка чистых фрагментов".into(),
                source: Source::Final,
                edited: false,
                lang: None,
            });
            transcript.rename_speaker(id, name);
            if id != "me" {
                transcript
                    .ktalk_sources
                    .insert(id.into(), "recording".into());
            }
        }
        meeting.write_transcript(&transcript).unwrap();
        library
            .write_meeting_voices(
                &meeting,
                BTreeMap::from([("old-cluster".into(), vec![0.0, 0.0, 1.0])]),
            )
            .unwrap();
        let mut seen = Vec::new();
        let count = remember_named_voices(&meeting, &transcript, |wav, ranges| {
            let mic = wav.file_name().unwrap() == "Mic.wav";
            let first = ranges[0].start_ms;
            seen.push((mic, ranges.clone()));
            Ok(if mic {
                vec![1.0, 0.0, 0.0]
            } else if first == 0 {
                vec![0.0, 1.0, 0.0]
            } else {
                vec![0.0, 0.0, 1.0]
            })
        })
        .unwrap();
        assert_eq!(count, 3);
        let voices = library.read_voices().unwrap();
        assert_eq!(voices.voices.len(), 3);
        assert!(
            voices
                .voices
                .iter()
                .any(|voice| voice.name == "Олег Сидоров" && voice.is_me)
        );
        assert!(voices.voices.iter().any(|voice| voice.name == "Гость"));
        for (mic, ranges) in seen {
            if !mic {
                assert!(
                    ranges
                        .iter()
                        .all(|range| range.end_ms <= 3000 || range.start_ms >= 4000)
                );
            }
        }
    }
    #[test]
    fn deleted_audio_never_calls_embed_or_creates_voice_state() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Без аудио", chrono::Utc::now()))
            .unwrap();
        let mut called = false;
        assert!(
            remember_named_voices(&meeting, &Transcript::default(), |_, _| {
                called = true;
                Ok(vec![1.0])
            })
            .is_err()
        );
        assert!(!called);
        assert!(library.read_voices().unwrap().voices.is_empty());
    }
}

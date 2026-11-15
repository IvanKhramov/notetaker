//! Validated Tolk word intervals are the only automatic enrollment evidence.
use anyhow::{Context, Result, ensure};
use notetaker_asr_proto::TimeRange;
use notetaker_core::{Channel, Library, Meeting, Transcript, VoiceEvidence, VoiceSample};

pub struct Enrollment {
    pub count: usize,
    pub skipped: Vec<String>,
}
pub fn remember_named_voices(
    meeting: &Meeting,
    transcript: &Transcript,
    evidence: &[VoiceEvidence],
    own_allowed: bool,
    expected: notetaker_core::Revision,
    mut embed: impl FnMut(&std::path::Path, Vec<TimeRange>) -> Result<Vec<f32>>,
    cancelled: impl Fn() -> bool,
) -> Result<Enrollment> {
    ensure!(
        meeting.mic_audio_path().is_file() || meeting.system_audio_path().is_file(),
        "Аудио не хранится: голоса не запомнены"
    );
    let mut report = Enrollment {
        count: 0,
        skipped: Vec::new(),
    };
    if evidence.iter().any(|voice| !voice.registered) {
        report
            .skipped
            .push("Гости запоминаются только после ручного именования".into());
    }
    let work = tempfile::tempdir()?;
    let mut samples = Vec::new();
    for (channel, source) in [
        (Channel::System, meeting.system_audio_path()),
        (Channel::Mic, meeting.mic_audio_path()),
    ] {
        let voices: Vec<_> = evidence
            .iter()
            .filter(|voice| {
                voice.registered
                    && voice.track == Some(channel)
                    && (channel != Channel::Mic || own_allowed)
            })
            .collect();
        if voices.is_empty() {
            continue;
        }
        if !source.is_file() {
            report
                .skipped
                .push(format!("Аудио {channel:?} не хранится"));
            continue;
        }
        let wav = work.path().join(format!("{channel:?}.wav"));
        let bound = crate::decode::decode(&source, &wav, |_| {
            ensure!(!cancelled(), "Операция Толка отменена");
            Ok(())
        })? / 16;
        for voice in voices {
            let ranges: Vec<_> = voice
                .ranges
                .iter()
                .filter_map(|&(a, b)| {
                    let end = b.min(bound).min(a.saturating_add(15000));
                    (end.saturating_sub(a) >= 1500).then_some(TimeRange {
                        start_ms: a,
                        end_ms: end,
                    })
                })
                .take(8)
                .collect();
            if ranges
                .iter()
                .map(|range| range.end_ms - range.start_ms)
                .sum::<u64>()
                < 6000
            {
                report
                    .skipped
                    .push(format!("{}: меньше 6 с чистой речи", voice.name));
                continue;
            }
            let own = channel == Channel::Mic;
            let name = if own {
                transcript.speaker_names.get("me").unwrap_or(&voice.name)
            } else {
                &voice.name
            };
            samples.push(VoiceSample {
                speaker: voice.speaker.clone(),
                name: name.clone(),
                embedding: embed(&wav, ranges)?,
                own,
            });
        }
    }
    if evidence
        .iter()
        .any(|voice| voice.registered && voice.track.is_none())
    {
        report.skipped.push(
            "Владелец ключа не определён однозначно; автоматическое обучение пропущено".into(),
        );
    }
    ensure!(
        meeting.read_transcript()? == *transcript,
        "Транскрипт изменился: повторите обучение голосов"
    );
    ensure!(!cancelled(), "Операция Толка отменена");
    if !samples.is_empty() {
        let (count, skipped) =
            Library::open(meeting.path().parent().context("Нет библиотеки")?)?
                .remember_tolk_voices(meeting, transcript, samples, expected, &cancelled)?;
        report.count = count;
        report.skipped.extend(skipped);
    }
    Ok(report)
}

/// Explicit manual naming can cache a guest's validated times without auto-enrolling it.
pub fn cache_named_voice(
    meeting: &Meeting,
    expected: &Transcript,
    speaker: &str,
    ranges: &[(u64, u64)],
    mut embed: impl FnMut(&std::path::Path, Vec<TimeRange>) -> Result<Vec<f32>>,
    cancelled: impl Fn() -> bool,
) -> Result<()> {
    ensure!(
        meeting.system_audio_path().is_file(),
        "Аудио не хранится: голос не запомнен"
    );
    let work = tempfile::tempdir()?;
    let wav = work.path().join("System.wav");
    let bound = crate::decode::decode(&meeting.system_audio_path(), &wav, |_| {
        ensure!(!cancelled(), "Операция Толка отменена");
        Ok(())
    })? / 16;
    let ranges: Vec<_> = ranges
        .iter()
        .filter_map(|&(a, b)| {
            let end = b.min(bound).min(a.saturating_add(15000));
            (end.saturating_sub(a) >= 1500).then_some(TimeRange {
                start_ms: a,
                end_ms: end,
            })
        })
        .take(8)
        .collect();
    ensure!(
        ranges.iter().map(|r| r.end_ms - r.start_ms).sum::<u64>() >= 6000,
        "Меньше 6 с чистой речи для голоса"
    );
    let library = Library::open(meeting.path().parent().context("Нет библиотеки")?)?;
    let embedding = embed(&wav, ranges)?;
    // Merge only the explicit sample; unrelated manual cluster samples remain valid.
    ensure!(
        meeting.read_transcript()? == *expected,
        "Транскрипт изменился: повторите именование"
    );
    ensure!(!cancelled(), "Операция Толка отменена");
    library.cache_speaker_voice(meeting, speaker, embedding)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{MeetingMeta, Segment, Source, SpeakerId};
    fn fixture() -> (tempfile::TempDir, Library, Meeting, Transcript) {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Нейтральная встреча", chrono::Utc::now()))
            .unwrap();
        for path in [meeting.system_audio_path(), meeting.mic_audio_path()] {
            let mut wav = hound::WavWriter::create(
                path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 16000,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for _ in 0..320_000 {
                wav.write_sample(0_i16).unwrap();
            }
            wav.finalize().unwrap();
        }
        let mut transcript = Transcript::default();
        transcript.rename_speaker("me", "Олег Сидоров");
        transcript.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId("me".into()),
            channel: Channel::Mic,
            start_ms: 0,
            end_ms: 8000,
            text: "Собственный голос".into(),
            source: Source::Final,
            edited: false,
            lang: None,
        });
        meeting.write_transcript(&transcript).unwrap();
        (root, library, meeting, transcript)
    }
    fn evidence(id: &str, registered: bool) -> VoiceEvidence {
        VoiceEvidence {
            speaker: id.into(),
            name: "Олег Сидоров".into(),
            registered,
            track: Some(Channel::System),
            ranges: vec![(0, 7000)],
        }
    }
    #[test]
    fn registered_namesakes_are_skipped_without_polluting_the_own_voice() {
        let (_root, library, meeting, transcript) = fixture();
        library
            .write_meeting_voices(
                &meeting,
                std::collections::BTreeMap::from([("me".into(), vec![1.0, 0.0, 0.0])]),
            )
            .unwrap();
        library
            .remember_speakers_owned(&meeting, &transcript, true)
            .unwrap();
        let before = library.read_voices().unwrap();
        let report = remember_named_voices(
            &meeting,
            &transcript,
            &[evidence("ktalk-a", true), evidence("ktalk-b", true)],
            false,
            meeting.load_transcript().unwrap().revision,
            |_, _| Ok(vec![0.0, 1.0, 0.0]),
            || false,
        )
        .unwrap();
        assert_eq!(report.count, 0);
        assert_eq!(report.skipped.len(), 2);
        assert!(
            report
                .skipped
                .iter()
                .all(|reason| reason.contains(notetaker_core::VOICE_NAME_TAKEN))
        );
        assert_eq!(library.read_voices().unwrap(), before);
    }
    #[test]
    fn guests_with_the_same_name_create_no_voice_records_and_foreign_mic_is_ignored() {
        let (_root, library, meeting, transcript) = fixture();
        let mut called = false;
        let report = remember_named_voices(
            &meeting,
            &transcript,
            &[
                evidence("ktalk-guest-a", false),
                evidence("ktalk-guest-b", false),
            ],
            false,
            meeting.load_transcript().unwrap().revision,
            |_, _| {
                called = true;
                Ok(vec![1.0])
            },
            || false,
        )
        .unwrap();
        assert_eq!(report.count, 0);
        assert!(!called);
        assert!(library.read_voices().unwrap().voices.is_empty());
    }
    #[test]
    fn owner_intervals_never_reach_system_embed_or_create_a_ktalk_record() {
        let (_root, library, meeting, transcript) = fixture();
        let mut owner = evidence("ktalk-owner", true);
        owner.track = Some(Channel::Mic);
        owner.ranges = vec![(0, 7000)];
        let mut peer = evidence("ktalk-peer", true);
        peer.name = "Анна Петрова".into();
        peer.ranges = vec![(10000, 17000)];
        let mut channels = Vec::new();
        let id = meeting.load_transcript().unwrap().revision;
        let result = remember_named_voices(
            &meeting,
            &transcript,
            &[owner, peer],
            true,
            id,
            |path, ranges| {
                if path.file_name().unwrap() == "System.wav" {
                    assert!(ranges.iter().all(|range| range.start_ms >= 10000));
                    channels.push(Channel::System);
                    Ok(vec![0.0, 1.0])
                } else {
                    assert_eq!(path.file_name().unwrap(), "Mic.wav");
                    assert_eq!(ranges[0].start_ms, 0);
                    channels.push(Channel::Mic);
                    Ok(vec![1.0, 0.0])
                }
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.count, 2);
        assert!(channels.contains(&Channel::Mic) && channels.contains(&Channel::System));
        let bank = library.read_voices().unwrap();
        assert!(
            !bank
                .voices
                .iter()
                .any(|voice| voice.id.as_deref() == Some("ktalk-owner"))
        );
        assert_eq!(bank.voices.iter().filter(|voice| voice.is_me).count(), 1);
    }
    #[test]
    fn trusted_microphone_uses_the_own_identity_and_short_evidence_is_skipped() {
        let (_root, library, meeting, transcript) = fixture();
        let mut short = evidence("ktalk-short", true);
        short.ranges = vec![(0, 5000)];
        let mut owner = evidence("ktalk-owner", true);
        owner.track = Some(Channel::Mic);
        owner.ranges = vec![(0, 8000)];
        let report = remember_named_voices(
            &meeting,
            &transcript,
            &[short, owner],
            true,
            meeting.load_transcript().unwrap().revision,
            |path, ranges| {
                assert_eq!(path.file_name().unwrap(), "Mic.wav");
                assert!(ranges.iter().all(|r| r.end_ms - r.start_ms <= 15000));
                Ok(vec![1.0, 0.0])
            },
            || false,
        )
        .unwrap();
        assert_eq!(report.count, 1);
        assert!(report.skipped[0].contains("меньше 6"));
        assert!(library.read_voices().unwrap().voices[0].is_me);
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
            remember_named_voices(
                &meeting,
                &Transcript::default(),
                &[],
                false,
                meeting.load_transcript().unwrap().revision,
                |_, _| {
                    called = true;
                    Ok(vec![1.0])
                },
                || false
            )
            .is_err()
        );
        assert!(!called);
        assert!(library.read_voices().unwrap().voices.is_empty());
    }
    #[test]
    fn cancellation_after_extraction_writes_no_biometric_records() {
        let (_root, library, meeting, transcript) = fixture();
        let cancelled = std::cell::Cell::new(false);
        let result = remember_named_voices(
            &meeting,
            &transcript,
            &[evidence("ktalk-a", true)],
            false,
            meeting.load_transcript().unwrap().revision,
            |_, _| {
                cancelled.set(true);
                Ok(vec![1.0, 0.0])
            },
            || cancelled.get(),
        );
        assert!(result.is_err());
        assert!(library.read_voices().unwrap().voices.is_empty());
    }
    #[test]
    fn explicit_guest_naming_can_use_validated_cached_times() {
        let (_root, library, meeting, mut transcript) = fixture();
        transcript.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId("ktalk-guest".into()),
            channel: Channel::System,
            start_ms: 0,
            end_ms: 7000,
            text: "Речь гостя".into(),
            source: Source::Final,
            edited: false,
            lang: None,
        });
        transcript.rename_speaker("ktalk-guest", "Гость");
        meeting.write_transcript(&transcript).unwrap();
        cache_named_voice(
            &meeting,
            &transcript,
            "ktalk-guest",
            &[(0, 7000)],
            |_, _| Ok(vec![0.0, 1.0]),
            || false,
        )
        .unwrap();
        assert!(library.read_voices().unwrap().voices.is_empty());
        library
            .edit_speakers(
                &meeting,
                &transcript,
                notetaker_core::SpeakerEdit::Rename {
                    speaker: "ktalk-guest".into(),
                    name: "Нейтральный гость".into(),
                    is_me: false,
                    everywhere: false,
                },
                true,
            )
            .unwrap();
        assert_eq!(
            library.read_voices().unwrap().voices[0].name,
            "Нейтральный гость"
        );
    }
}

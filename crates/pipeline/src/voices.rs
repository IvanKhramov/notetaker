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
                recording_id: voice.recording_id.clone(),
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

/// Extract a guest's embedding without writing biometric state.
pub fn named_voice_embedding(
    meeting: &Meeting,
    expected: &Transcript,
    ranges: &[(u64, u64)],
    mut embed: impl FnMut(&std::path::Path, Vec<TimeRange>) -> Result<Vec<f32>>,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<f32>> {
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
    let embedding = embed(&wav, ranges)?;
    ensure!(
        meeting.read_transcript()? == *expected,
        "Транскрипт изменился: повторите именование"
    );
    ensure!(!cancelled(), "Операция Толка отменена");
    Ok(embedding)
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
            recording_id: None,
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
        let embedding = named_voice_embedding(
            &meeting,
            &transcript,
            &[(0, 7000)],
            |_, _| Ok(vec![0.0, 1.0]),
            || false,
        )
        .unwrap();
        assert!(library.read_voices().unwrap().voices.is_empty());
        library
            .edit_speakers_owned(
                &meeting,
                &transcript,
                notetaker_core::SpeakerEdit::Rename {
                    speaker: "ktalk-guest".into(),
                    name: "Нейтральный гость".into(),
                    is_me: false,
                    everywhere: false,
                },
                true,
                false,
                Some(embedding),
            )
            .unwrap();
        assert_eq!(
            library.read_voices().unwrap().voices[0].name,
            "Нейтральный гость"
        );
    }
}

/// Downloads, PCM and failures are scoped to one TempDir, including cancellation.
pub struct HistoricalEnrollment {
    pub people: Vec<String>,
    pub no_speech: bool,
}
pub fn learn_recording(
    library: &Library,
    source: &str,
    evidence: &[VoiceEvidence],
    download: impl FnOnce(&mut std::fs::File) -> Result<()>,
    mut embed: impl FnMut(&std::path::Path, Vec<TimeRange>) -> Result<Vec<f32>>,
    cancelled: impl Fn() -> bool,
) -> Result<HistoricalEnrollment> {
    anyhow::ensure!(
        !evidence.iter().any(|v| v.registered && v.track.is_none()),
        "Владелец ключа не определён однозначно"
    );
    let mut candidates = Vec::new();
    let mut known = Vec::new();
    for voice in evidence
        .iter()
        .filter(|v| v.registered && v.track.is_some())
    {
        if library.learned_recording_identity(source, &voice.speaker)? {
            known.push(voice.speaker.clone());
        } else if voice
            .ranges
            .iter()
            .map(|(a, b)| b.saturating_sub(*a).min(15000))
            .sum::<u64>()
            >= 6000
        {
            candidates.push(voice);
        }
    }
    if candidates.is_empty() {
        return Ok(HistoricalEnrollment {
            no_speech: known.is_empty(),
            people: known,
        });
    }
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("recording.mp4");
    let mut file = std::fs::File::create(&path)?;
    download(&mut file)?;
    drop(file);
    anyhow::ensure!(!cancelled(), "Обучение голосов отменено");
    let wav = temp.path().join("recording.wav");
    let bound = crate::decode::decode_native(&path, &wav, &mut |_| {
        anyhow::ensure!(!cancelled(), "Обучение голосов отменено");
        Ok(())
    })? / 16;
    let mut samples = Vec::new();
    for voice in candidates {
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
        if ranges.iter().map(|r| r.end_ms - r.start_ms).sum::<u64>() < 6000 {
            continue;
        }
        let embedding = embed(&wav, ranges)?;
        anyhow::ensure!(!cancelled(), "Обучение голосов отменено");
        samples.push(VoiceSample {
            recording_id: voice.recording_id.clone(),
            speaker: voice.speaker.clone(),
            name: voice.name.clone(),
            own: voice.track == Some(Channel::Mic),
            embedding,
        });
    }
    known.extend(library.learn_recording_voices(source, samples, cancelled)?);
    Ok(HistoricalEnrollment {
        no_speech: known.is_empty(),
        people: known,
    })
}

#[cfg(test)]
mod historical_tests {
    use super::*;
    fn evidence() -> VoiceEvidence {
        VoiceEvidence {
            recording_id: Some("recording".into()),
            speaker: "ktalk-person".into(),
            name: "Анна Петрова".into(),
            registered: true,
            track: Some(Channel::System),
            ranges: vec![(0, 8000)],
        }
    }
    #[test]
    fn native_mp4_learning_is_idempotent_and_temp_media_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/voice-learning.mp4");
        let mut work = None;
        let report = learn_recording(
            &library,
            "source",
            &[evidence()],
            |out| {
                std::io::copy(&mut std::fs::File::open(&source)?, out)?;
                Ok(())
            },
            |wav, ranges| {
                assert_eq!(ranges.len(), 1);
                let input = hound::WavReader::open(wav)?;
                assert_eq!(input.spec().sample_rate, 16000);
                assert_eq!(input.spec().channels, 1);
                work = Some(wav.parent().unwrap().to_path_buf());
                Ok(vec![1.0, 0.0])
            },
            || false,
        )
        .unwrap();
        assert_eq!(report.people, vec!["ktalk-person"]);
        assert!(!work.unwrap().exists());
        let before = library.read_voices().unwrap();
        let report = learn_recording(
            &library,
            "source",
            &[evidence()],
            |_| panic!("already learned: no download"),
            |_, _| panic!("already learned: no ASR"),
            || false,
        )
        .unwrap();
        assert_eq!(report.people, vec!["ktalk-person"]);
        assert!(!report.no_speech);
        assert_eq!(library.read_voices().unwrap(), before);
    }
    #[test]
    fn failure_and_cancel_remove_temporary_audio_and_ambiguous_owner_never_embeds() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/voice-learning.mp4");
        for cancelled in [false, true] {
            let mut work = None;
            let flag = std::cell::Cell::new(false);
            let result = learn_recording(
                &library,
                "failed",
                &[evidence()],
                |out| {
                    std::io::copy(&mut std::fs::File::open(&source)?, out)?;
                    Ok(())
                },
                |wav, _| {
                    work = Some(wav.parent().unwrap().to_path_buf());
                    if cancelled {
                        flag.set(true);
                        Ok(vec![1.0, 0.0])
                    } else {
                        anyhow::bail!("synthetic failure")
                    }
                },
                || flag.get(),
            );
            assert!(result.is_err());
            assert!(!work.unwrap().exists());
            assert!(library.read_voices().unwrap().voices.is_empty());
        }
        let mut owner = evidence();
        owner.track = None;
        assert!(
            learn_recording(
                &library,
                "ambiguous",
                &[owner],
                |_| panic!("no download for ambiguous owner"),
                |_, _| panic!("no ASR"),
                || false
            )
            .is_err()
        );
        let mut guest = evidence();
        guest.registered = false;
        assert!(
            learn_recording(
                &library,
                "guest",
                &[guest],
                |_| panic!("guests are not learned"),
                |_, _| panic!("guests are not learned"),
                || false
            )
            .unwrap()
            .no_speech
        );
    }
}

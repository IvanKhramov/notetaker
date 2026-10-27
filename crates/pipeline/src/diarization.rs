use std::{collections::BTreeMap, path::Path, sync::mpsc::Sender};

use anyhow::{Context, Result};
use notetaker_asr_proto::{Diarization, TimeRange};
use notetaker_core::{Channel, Library, Meeting, SELF_THRESHOLD, Transcript};

use crate::{Asr, PipelineEvent};

#[derive(Clone, Debug)]
pub struct DiarizationConfig {
    pub enabled: bool,
    pub speakers: Option<u32>,
    pub voice_library: bool,
}
impl Default for DiarizationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            speakers: None,
            voice_library: false,
        }
    }
}

fn assign(transcript: &mut Transcript, result: &Diarization) {
    for segment in &mut transcript.segments {
        if segment.channel != Channel::System {
            continue;
        }
        let mut overlap = BTreeMap::<&str, u64>::new();
        for turn in &result.turns {
            let ms = segment
                .end_ms
                .min(turn.end_ms)
                .saturating_sub(segment.start_ms.max(turn.start_ms));
            *overlap.entry(&turn.speaker).or_default() += ms;
        }
        if let Some((speaker, _)) = overlap
            .into_iter()
            .filter(|(_, ms)| *ms > 0)
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        {
            segment.speaker.0 = speaker.into();
        }
    }
}

/// ASR is already durable. A missing model, crash, timeout or local voice I/O
/// must never turn a successfully transcribed recording into a failed one.
pub(crate) fn final_pass(
    meeting: &Meeting,
    asr: &mut impl Asr,
    system: &Path,
    mic: Option<&Path>,
    config: &DiarizationConfig,
    events: &Sender<PipelineEvent>,
) {
    if !config.enabled {
        return;
    }
    let warning = |error: anyhow::Error| {
        let _ = events.send(PipelineEvent::Warning(format!(
            "Разделение собеседников: {error:#}"
        )));
    };
    let mut result = match asr.diarize(system, config.speakers, config.voice_library) {
        Ok(result) => result,
        Err(error) => {
            warning(error);
            return;
        }
    };
    let mut suggestions = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut voice_ids = BTreeMap::new();
    if config.voice_library {
        let local = (|| -> Result<()> {
            let library =
                Library::open(meeting.path().parent().context("meeting has no library")?)?;
            let voices = library.prepare_voices()?;
            let mut self_ids = Vec::new();
            for (id, embedding) in &result.embeddings {
                if let Some((voice, score)) = voices.suggest(embedding) {
                    if mic.is_none() && voice.is_me && score >= SELF_THRESHOLD {
                        if let Some(voice_id) = &voice.id {
                            voice_ids.insert("me".into(), voice_id.clone());
                        }
                        self_ids.push(id.clone());
                        names.insert("me".to_owned(), voice.name.clone());
                    } else {
                        if let Some(voice_id) = &voice.id {
                            voice_ids.insert(id.clone(), voice_id.clone());
                        }
                        suggestions.insert(id.clone(), voice.name.clone());
                    }
                }
            }
            for id in self_ids {
                for turn in &mut result.turns {
                    if turn.speaker == id {
                        turn.speaker = "me".into();
                    }
                }
                if let Some(embedding) = result.embeddings.remove(&id) {
                    result.embeddings.entry("me".into()).or_insert(embedding);
                }
            }
            if let Some(mic) = mic {
                let transcript = meeting.read_transcript()?;
                let ranges = transcript
                    .segments
                    .iter()
                    .filter(|s| s.channel == Channel::Mic)
                    .map(|s| TimeRange {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                    })
                    .collect();
                match asr.embed(mic, ranges) {
                    Ok(embedding) => {
                        if let Some((voice, _)) = voices.suggest(&embedding) {
                            if let Some(voice_id) = &voice.id {
                                voice_ids.insert("me".into(), voice_id.clone());
                            }
                            suggestions.insert("me".into(), voice.name.clone());
                        }
                        result.embeddings.insert("me".into(), embedding);
                    }
                    Err(error) => warning(error),
                }
            }
            library.write_meeting_voices(meeting, result.embeddings.clone())?;
            Ok(())
        })();
        if let Err(error) = local {
            warning(error);
        }
    }
    if let Err(error) = meeting.update_transcript(|transcript| {
        assign(transcript, &result);
        transcript.speaker_suggestions.extend(suggestions);
        transcript.speaker_names.extend(names);
        transcript.speaker_voice_ids.extend(voice_ids);
    }) {
        warning(error.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_asr_proto::SpeakerTurn;
    use notetaker_core::{Segment, Source, SpeakerId};
    #[test]
    fn overlap_sums_turns_and_preserves_uncovered_and_mic() {
        let mut transcript = Transcript::default();
        for (channel, start, end) in [
            (Channel::System, 0, 1000),
            (Channel::Mic, 0, 1000),
            (Channel::System, 2000, 3000),
        ] {
            transcript.segments.push(Segment {
                id: uuid::Uuid::now_v7(),
                speaker: SpeakerId(
                    if channel == Channel::Mic {
                        "me"
                    } else {
                        "them"
                    }
                    .into(),
                ),
                channel,
                start_ms: start,
                end_ms: end,
                text: "text".into(),
                source: Source::Final,
                edited: false,
                lang: Some("ru".into()),
            });
        }
        assign(
            &mut transcript,
            &Diarization {
                turns: vec![
                    SpeakerTurn {
                        start_ms: 0,
                        end_ms: 300,
                        speaker: "s1".into(),
                    },
                    SpeakerTurn {
                        start_ms: 300,
                        end_ms: 700,
                        speaker: "s2".into(),
                    },
                    SpeakerTurn {
                        start_ms: 700,
                        end_ms: 1000,
                        speaker: "s1".into(),
                    },
                ],
                ..Default::default()
            },
        );
        assert!(
            transcript
                .segments
                .iter()
                .all(|s| s.lang.as_deref() == Some("ru"))
        );
        assert_eq!(
            transcript
                .segments
                .iter()
                .map(|s| s.speaker.0.as_str())
                .collect::<Vec<_>>(),
            ["s1", "me", "them"]
        );
    }
    struct Fake {
        fail: bool,
    }
    impl Asr for Fake {
        fn start(&mut self) -> Result<()> {
            Ok(())
        }
        fn transcribe(
            &mut self,
            _: notetaker_asr_proto::AudioInput,
            _: notetaker_capture::Channel,
            _: u64,
        ) -> Result<Vec<notetaker_asr_proto::Segment>> {
            Ok(Vec::new())
        }
        fn diarize(&mut self, _: &Path, _: Option<u32>, embeddings: bool) -> Result<Diarization> {
            if self.fail {
                anyhow::bail!("missing model or crashed worker");
            }
            Ok(Diarization {
                turns: vec![SpeakerTurn {
                    start_ms: 0,
                    end_ms: 2000,
                    speaker: "s1".into(),
                }],
                embeddings: if embeddings {
                    BTreeMap::from([("s1".into(), vec![1.0, 0.0])])
                } else {
                    BTreeMap::new()
                },
            })
        }
    }
    #[test]
    fn final_pass_preserves_asr_on_failure_and_keeps_biometrics_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Fallback",
                chrono::Utc::now(),
            ))
            .unwrap();
        let mut transcript = Transcript::default();
        transcript.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId("them".into()),
            channel: Channel::System,
            start_ms: 0,
            end_ms: 1000,
            text: "Evidence".into(),
            source: Source::Final,
            edited: true,
            lang: None,
        });
        meeting.write_transcript(&transcript).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        final_pass(
            &meeting,
            &mut Fake { fail: true },
            Path::new("unused"),
            None,
            &DiarizationConfig::default(),
            &tx,
        );
        assert_eq!(meeting.read_transcript().unwrap(), transcript);
        assert!(matches!(rx.try_recv().unwrap(), PipelineEvent::Warning(_)));
        final_pass(
            &meeting,
            &mut Fake { fail: false },
            Path::new("unused"),
            None,
            &DiarizationConfig::default(),
            &tx,
        );
        assert_eq!(
            meeting.read_transcript().unwrap().segments[0].speaker.0,
            "s1"
        );
        assert!(!dir.path().join(".notetaker").exists());
    }
    #[test]
    fn imported_self_is_recognized_from_local_voice_library() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(notetaker_core::MeetingMeta::new(
                "Import",
                chrono::Utc::now(),
            ))
            .unwrap();
        let mut transcript = Transcript::default();
        transcript.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId("them".into()),
            channel: Channel::System,
            start_ms: 0,
            end_ms: 1000,
            text: "Evidence".into(),
            source: Source::Final,
            edited: false,
            lang: None,
        });
        meeting.write_transcript(&transcript).unwrap();
        library
            .write_meeting_voices(&meeting, BTreeMap::from([("me".into(), vec![1.0, 0.0])]))
            .unwrap();
        let mut enrollment = Transcript::default();
        enrollment.rename_speaker("me", "Иван");
        library.remember_speakers(&meeting, &enrollment).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        final_pass(
            &meeting,
            &mut Fake { fail: false },
            Path::new("unused"),
            None,
            &DiarizationConfig {
                voice_library: true,
                ..Default::default()
            },
            &tx,
        );
        let loaded = meeting.read_transcript().unwrap();
        assert_eq!(loaded.segments[0].speaker.0, "me");
        assert_eq!(loaded.speaker_name("me"), "Иван");
        assert_eq!(
            loaded.speaker_voice_ids.get("me"),
            library.read_voices().unwrap().voices[0].id.as_ref()
        );
    }
}

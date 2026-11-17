//! Online identities are session-local; only the final clusters can be enrolled.
use crate::{AsrConfig, PipelineEvent};
use anyhow::Result;
use notetaker_asr_proto::{AsrClient, CancellationHandle, TimeRange};
use notetaker_core::{Channel, Meeting, SpeakerEdit, Transcript, cosine};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread::JoinHandle,
    time::Duration,
};

// Initial calibration supplied for live meetings; keep independent of library thresholds.
pub const JOIN_COSINE: f32 = 0.60;
pub const MIN_SPEECH_MS: u64 = 1500;
pub const UPDATE_CENTROID_MS: u64 = 3000;
// A brief answer can inherit only the immediately preceding voice within one second.
pub const NEIGHBOR_GAP_MS: u64 = 1000;
pub const QUEUE_CAPACITY: usize = 4;
pub(crate) type Edits = std::sync::Arc<std::sync::Mutex<Vec<SpeakerEdit>>>;
pub(crate) fn edit_id(edit: &SpeakerEdit) -> Option<&str> {
    match edit {
        SpeakerEdit::Rename {
            speaker,
            name,
            everywhere: false,
            ..
        } if !name.trim().is_empty() && speaker.starts_with("live-") => Some(speaker),
        SpeakerEdit::RejectVoice { speaker } | SpeakerEdit::ClearName { speaker }
            if speaker.starts_with("live-") =>
        {
            Some(speaker)
        }
        _ => None,
    }
}
pub(crate) fn apply_pending(transcript: &mut Transcript, edits: &Edits) {
    for edit in edits.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        apply_edit(transcript, edit);
    }
}
#[derive(Default)]
pub(crate) struct Clusters {
    centroids: BTreeMap<String, Vec<f32>>,
    counts: BTreeMap<String, usize>,
    previous: Option<(String, u64)>,
}
impl Clusters {
    fn assign(&mut self, start: u64, end: u64, embedding: Option<Vec<f32>>) -> String {
        let duration = end.saturating_sub(start);
        if duration < MIN_SPEECH_MS {
            let id = self
                .previous
                .as_ref()
                .filter(|(_, last)| start.saturating_sub(*last) <= NEIGHBOR_GAP_MS)
                .map_or("them".into(), |(id, _)| id.clone());
            if id != "them" {
                self.previous = Some((id.clone(), end));
            }
            return id;
        }
        let Some(embedding) = embedding.filter(|e| cosine(e, e).is_some()) else {
            return "them".into();
        };
        let best = self
            .centroids
            .iter()
            .filter_map(|(id, c)| cosine(c, &embedding).map(|score| (id.clone(), score)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let id = match best.filter(|(_, score)| *score >= JOIN_COSINE) {
            Some((id, _)) => id,
            // A short utterance is too noisy to open a new speaker.
            None if duration < UPDATE_CENTROID_MS => return "them".into(),
            None => format!("live-{}", self.centroids.len() + 1),
        };
        if !self.centroids.contains_key(&id) {
            self.centroids.insert(id.clone(), embedding.clone());
            self.counts.insert(id.clone(), 1);
        } else if duration >= UPDATE_CENTROID_MS {
            let count = self.counts.entry(id.clone()).or_default();
            if let Some(centroid) = self.centroids.get_mut(&id) {
                for (a, b) in centroid.iter_mut().zip(&embedding) {
                    *a = (*a * *count as f32 + b) / (*count + 1) as f32;
                }
            }
            *count += 1;
        }
        self.previous = Some((id.clone(), end));
        id
    }
    pub(crate) fn has_named(&self, before: &Transcript) -> bool {
        self.centroids
            .keys()
            .any(|id| before.speaker_names.contains_key(id))
    }
    pub(crate) fn transfer(
        &self,
        before: &Transcript,
        final_embeddings: &BTreeMap<String, Vec<f32>>,
    ) -> Vec<SpeakerEdit> {
        final_embeddings
            .iter()
            .filter(|(id, _)| id.as_str() != "me")
            .filter_map(|(target, embedding)| {
                let id = self.match_live(before, embedding, false)?;
                Some(SpeakerEdit::Rename {
                    speaker: target.clone(),
                    name: before.speaker_names.get(id)?.clone(),
                    is_me: before.self_speaker.as_deref() == Some(id),
                    everywhere: false,
                })
            })
            .collect()
    }
    fn match_live<'a>(
        &'a self,
        before: &Transcript,
        embedding: &[f32],
        rejected: bool,
    ) -> Option<&'a str> {
        let mut ranked: Vec<_> = self
            .centroids
            .iter()
            .filter(|(id, _)| {
                if rejected {
                    true
                } else {
                    before.speaker_names.contains_key(*id)
                }
            })
            .filter_map(|(id, c)| cosine(c, embedding).map(|score| (id.as_str(), score)))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (id, score) = ranked.first()?;
        if (!rejected || before.voice_rejected.contains(*id))
            && *score >= notetaker_core::AUTOMATIC_THRESHOLD
            && ranked
                .get(1)
                .is_none_or(|second| score - second.1 >= notetaker_core::AUTOMATIC_MARGIN)
        {
            Some(*id)
        } else {
            notetaker_runtime::diagnostics::event(
                "live speaker identity transfer ambiguous or weak",
            );
            None
        }
    }
    pub(crate) fn rejected_matches(
        &self,
        before: &Transcript,
        embeddings: &BTreeMap<String, Vec<f32>>,
    ) -> Vec<String> {
        embeddings
            .iter()
            .filter(|(id, _)| id.as_str() != "me")
            .filter(|(_, e)| self.match_live(before, e, true).is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }
}
struct Job {
    segment: uuid::Uuid,
    start: u64,
    end: u64,
    samples: std::sync::Arc<Vec<f32>>,
    range: std::ops::Range<usize>,
}
pub(crate) struct Worker {
    tx: Option<SyncSender<Job>>,
    cancel: CancellationHandle,
    thread: Option<JoinHandle<Clusters>>,
}
impl Worker {
    pub(crate) fn start(
        config: AsrConfig,
        meeting: Meeting,
        diarization: crate::DiarizationConfig,
        events: std::sync::mpsc::Sender<PipelineEvent>,
        cancel: CancellationHandle,
        edits: Edits,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
        let control = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("live-speakers".into())
            .spawn(move || {
                let mut clusters = Clusters::default();
                let result = (|| -> Result<()> {
                    let mut command = std::process::Command::new(&config.binary);
                    command.env(
                        "NOTETAKER_MODELS_DIR",
                        notetaker_core::app_config::AppSettings::load()?.models_path(),
                    );
                    let mut client = AsrClient::spawn_command_cancellable(
                        command,
                        Duration::from_secs(30),
                        control.clone(),
                    )?;
                    let work = tempfile::tempdir_in(meeting.path().join("audio/.work"))?;
                    let library = notetaker_core::Library::open(
                        meeting
                            .path()
                            .parent()
                            .ok_or_else(|| anyhow::anyhow!("Нет библиотеки"))?,
                    )?;
                    let bank = if diarization.voice_library {
                        Some(library.read_voices()?)
                    } else {
                        None
                    };
                    for job in rx {
                        if control.is_cancelled() {
                            break;
                        }
                        let duration = job.end.saturating_sub(job.start);
                        let embedding = if duration >= MIN_SPEECH_MS {
                            let path = work.path().join("utterance.wav");
                            let mut wav = hound::WavWriter::create(
                                &path,
                                hound::WavSpec {
                                    channels: 1,
                                    sample_rate: 16000,
                                    bits_per_sample: 32,
                                    sample_format: hound::SampleFormat::Float,
                                },
                            )?;
                            for sample in &job.samples[job.range] {
                                wav.write_sample(*sample)?;
                            }
                            wav.finalize()?;
                            match client.embed(
                                &path,
                                vec![TimeRange {
                                    start_ms: 0,
                                    end_ms: duration,
                                }],
                            ) {
                                Ok(e) => Some(e),
                                Err(_) => {
                                    notetaker_runtime::diagnostics::event(
                                        "live speaker embedding failed",
                                    );
                                    break;
                                }
                            }
                        } else {
                            None
                        };
                        if control.is_cancelled() {
                            break;
                        }
                        let speaker = clusters.assign(job.start, job.end, embedding);
                        let updated = meeting.update_transcript_versioned(|transcript| {
                            if let Some(segment) =
                                transcript.segments.iter_mut().find(|s| s.id == job.segment)
                            {
                                segment.speaker.0 = speaker.clone();
                            }
                            apply_pending(transcript, &edits);
                            if let Some(bank) = &bank {
                                bank.recognize(
                                    transcript,
                                    &clusters.centroids,
                                    diarization.automatic_voice_names,
                                );
                            }
                        })?;
                        let _ = events.send(PipelineEvent::LiveSpeakers(updated.value));
                    }
                    Ok(())
                })();
                if result.is_err() {
                    notetaker_runtime::diagnostics::event("live speaker worker stopped");
                }
                clusters
            })?;
        Ok(Self {
            tx: Some(tx),
            cancel,
            thread: Some(thread),
        })
    }
    pub(crate) fn enqueue(
        &self,
        segment: &notetaker_core::Segment,
        chunk_start: u64,
        samples: std::sync::Arc<Vec<f32>>,
    ) {
        let start = segment
            .start_ms
            .saturating_sub(chunk_start)
            .saturating_mul(16) as usize;
        let end = segment
            .end_ms
            .saturating_sub(chunk_start)
            .saturating_mul(16) as usize;
        if samples.get(start..end).is_none() {
            return;
        }
        if self.cancel.is_cancelled() {
            return;
        }
        if let Some(tx) = &self.tx {
            match tx.try_send(Job {
                segment: segment.id,
                start: segment.start_ms,
                end: segment.end_ms,
                samples,
                range: start..end,
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    notetaker_runtime::diagnostics::event("live speaker queue unavailable");
                }
            }
        }
    }
    pub(crate) fn finish(&mut self) -> Clusters {
        self.cancel.cancel();
        self.tx.take();
        self.thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Live edits only change transcript identity. Never read or write the biometric bank.
pub fn edit(meeting: &Meeting, edit: SpeakerEdit) -> Result<notetaker_core::Versioned<Transcript>> {
    anyhow::ensure!(
        edit_id(&edit).is_some(),
        "Действие недоступно во время записи"
    );
    meeting
        .update_transcript_versioned(|t| {
            apply_edit(t, &edit);
        })
        .map_err(Into::into)
}
pub fn apply_edit(t: &mut Transcript, edit: &SpeakerEdit) -> bool {
    let Some(speaker) = edit_id(edit) else {
        return false;
    };
    let speaker = speaker.to_owned();
    if !t
        .segments
        .iter()
        .any(|s| s.channel == Channel::System && s.speaker.0 == speaker)
    {
        return false;
    }
    match edit {
        SpeakerEdit::Rename { name, is_me, .. } => {
            // A live name identifies one cluster. The latest explicit assignment wins.
            let normalized = name.trim().to_lowercase();
            let previous: Vec<_> = t
                .speaker_identities()
                .filter(|(id, identity)| {
                    id.starts_with("live-")
                        && *id != speaker
                        && identity.name.trim().to_lowercase() == normalized
                })
                .map(|(id, _)| id.to_owned())
                .collect();
            for id in previous {
                apply_edit(t, &SpeakerEdit::ClearName { speaker: id });
            }
            if t.voice_recognitions
                .get(&speaker)
                .is_some_and(|mark| mark.name != name.trim())
            {
                t.voice_recognitions.remove(&speaker);
                t.speaker_voice_ids.remove(&speaker);
            }
            t.rename_speaker(&speaker, name);
            if *is_me {
                t.self_speaker = Some(speaker.clone());
            }
        }
        SpeakerEdit::RejectVoice { .. } => {
            t.reject_voice(&speaker);
            t.voice_rejected.insert(speaker.clone());
        }
        SpeakerEdit::ClearName { .. } => {
            t.speaker_names.remove(&speaker);
            t.voice_recognitions.remove(&speaker);
            t.speaker_suggestions.remove(&speaker);
            t.speaker_voice_ids.remove(&speaker);
            t.voice_rejected.insert(speaker.clone());
            if t.self_speaker.as_ref() == Some(&speaker) {
                t.self_speaker = None;
            }
        }
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Library, MeetingMeta, Segment, Source, SpeakerId};
    fn segment(id: &str, start: u64, end: u64) -> Segment {
        Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId(id.into()),
            channel: Channel::System,
            start_ms: start,
            end_ms: end,
            text: "Синтетическая реплика".into(),
            source: Source::Live,
            edited: false,
            lang: None,
        }
    }
    #[test]
    fn alternating_voices_short_answers_and_centroid_update_floor() {
        let mut c = Clusters::default();
        assert_eq!(c.assign(0, 3000, Some(vec![1.0, 0.0])), "live-1");
        assert_eq!(c.assign(3500, 6500, Some(vec![0.0, 1.0])), "live-2");
        assert_eq!(c.assign(6600, 7000, None), "live-2");
        assert_eq!(c.assign(7200, 7500, None), "live-2");
        assert_eq!(c.assign(9000, 9400, None), "them");
        assert_eq!(c.assign(10000, 12000, Some(vec![1.0, 0.1])), "live-1");
        assert_eq!(c.centroids["live-1"], vec![1.0, 0.0]);
        assert_eq!(c.assign(12100, 14000, Some(vec![-1.0, 0.0])), "them");
        assert_eq!(c.centroids.len(), 2);
        assert_eq!(c.assign(13000, 16000, Some(vec![1.0, 0.1])), "live-1");
        assert!(c.centroids["live-1"][1] > 0.0);
    }
    #[test]
    fn strict_transfer_rejects_ambiguity_missing_speakers_and_carries_rejection() {
        let mut c = Clusters::default();
        c.assign(0, 3000, Some(vec![1.0, 0.0]));
        c.assign(4000, 7000, Some(vec![0.0, 1.0]));
        let mut t = Transcript::default();
        t.speaker_names.insert("live-1".into(), "Участник А".into());
        t.speaker_names.insert("live-2".into(), "Участник Б".into());
        t.self_speaker = Some("live-1".into());
        let matched = c.transfer(&t, &BTreeMap::from([("s1".into(), vec![1.0, 0.0])]));
        assert!(
            matches!(&matched[0],SpeakerEdit::Rename {name,is_me:true,..} if name=="Участник А")
        );
        assert!(
            c.transfer(&t, &BTreeMap::from([("s1".into(), vec![1.0, 1.0])]))
                .is_empty()
        );
        assert!(c.transfer(&t, &BTreeMap::new()).is_empty());
        assert!(
            c.transfer(&t, &BTreeMap::from([("s1".into(), vec![-1.0, 0.0])]))
                .is_empty()
        );
        t.voice_rejected.insert("live-2".into());
        assert_eq!(
            c.rejected_matches(&t, &BTreeMap::from([("s2".into(), vec![0.0, 1.0])])),
            vec!["s2"]
        );
    }
    #[test]
    fn full_queue_and_cancellation_leave_jobs_unassigned() {
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = CancellationHandle::default();
        let mut worker = Worker {
            tx: Some(tx),
            cancel: cancel.clone(),
            thread: None,
        };
        let first = segment("them", 0, 3000);
        let samples = std::sync::Arc::new(vec![0.0; 48000]);
        worker.enqueue(&first, 0, samples.clone());
        worker.enqueue(&segment("them", 0, 3000), 0, samples.clone());
        assert_eq!(rx.try_recv().unwrap().segment, first.id);
        assert!(rx.try_recv().is_err());
        cancel.cancel();
        worker.enqueue(&first, 0, samples);
        assert!(rx.try_recv().is_err());
        worker.finish();
    }
    #[test]
    fn live_naming_changes_past_and_future_text_but_never_writes_voice_data() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new(
                "Синтетическая встреча",
                chrono::Utc::now(),
            ))
            .unwrap();
        let t = Transcript {
            segments: vec![segment("live-1", 0, 3000), segment("live-1", 4000, 7000)],
            ..Default::default()
        };
        meeting.write_transcript(&t).unwrap();
        edit(
            &meeting,
            SpeakerEdit::Rename {
                speaker: "live-1".into(),
                name: "Участник А".into(),
                is_me: false,
                everywhere: false,
            },
        )
        .unwrap();
        meeting
            .update_transcript(|t| t.segments.push(segment("live-1", 8000, 11000)))
            .unwrap();
        let t = meeting.read_transcript().unwrap();
        assert!(
            t.segments
                .iter()
                .all(|s| t.speaker_name(&s.speaker.0) == "Участник А")
        );
        assert!(!library.private_state_dir().join("voices.json").exists());
        edit(
            &meeting,
            SpeakerEdit::ClearName {
                speaker: "live-1".into(),
            },
        )
        .unwrap();
        assert!(
            !meeting
                .read_transcript()
                .unwrap()
                .speaker_names
                .contains_key("live-1")
        );
    }
    #[test]
    fn pending_edits_keep_user_order_for_own_voice_and_unique_live_names() {
        let mut clusters = Clusters::default();
        clusters.assign(0, 3000, Some(vec![1.0, 0.0]));
        clusters.assign(4000, 7000, Some(vec![0.0, 1.0]));
        let rename = |speaker: &str, name: &str, is_me| SpeakerEdit::Rename {
            speaker: speaker.into(),
            name: name.into(),
            is_me,
            everywhere: false,
        };
        let edits: Edits = Default::default();
        edits.lock().unwrap().extend([
            rename("live-2", "Участник Б", true),
            rename("live-1", "Участник А", true),
        ]);
        let mut t = Transcript {
            segments: vec![segment("live-1", 0, 3000), segment("live-2", 4000, 7000)],
            ..Default::default()
        };
        for _ in 0..2 {
            apply_pending(&mut t, &edits);
            assert_eq!(t.self_speaker.as_deref(), Some("live-1"));
            let transfer = clusters.transfer(
                &t,
                &BTreeMap::from([("s1".into(), vec![1.0, 0.0]), ("s2".into(), vec![0.0, 1.0])]),
            );
            assert!(
                matches!(&transfer[0], SpeakerEdit::Rename {speaker,is_me:true,..} if speaker=="s1")
            );
            assert!(
                matches!(&transfer[1], SpeakerEdit::Rename {speaker,is_me:false,..} if speaker=="s2")
            );
        }
        edits.lock().unwrap().extend([
            rename("live-2", "Участник В", false),
            rename("live-1", "  УЧАСТНИК В  ", false),
        ]);
        for _ in 0..2 {
            apply_pending(&mut t, &edits);
            assert!(!t.speaker_names.contains_key("live-2"));
            assert_eq!(t.speaker_names["live-1"], "УЧАСТНИК В");
            assert!(!t.speaker_voice_ids.contains_key("live-2"));
        }
        t.voice_rejected.remove("live-2");
        t.voice_recognitions.insert(
            "live-2".into(),
            notetaker_core::VoiceRecognition {
                name: "Участник В".into(),
                voice_id: "synthetic-voice".into(),
                similarity_milli: 900,
            },
        );
        apply_edit(&mut t, &rename("live-1", "Участник В", false));
        assert!(!t.voice_recognitions.contains_key("live-2"));
    }

    #[test]
    fn live_controls_survive_stop_without_a_late_disk_command() {
        let stop = crate::StopHandle::default();
        let rename = SpeakerEdit::Rename {
            speaker: "live-1".into(),
            name: "Участник А".into(),
            is_me: false,
            everywhere: false,
        };
        assert!(stop.name_live(rename.clone()));
        stop.stop();
        assert!(!stop.name_live(SpeakerEdit::ClearName {
            speaker: "live-1".into()
        }));
        let mut t = Transcript {
            segments: vec![segment("live-1", 0, 3000)],
            ..Default::default()
        };
        assert!(apply_edit(&mut t, &rename));
        assert_eq!(t.speaker_name("live-1"), "Участник А");
    }
    #[test]
    fn live_library_recognition_uses_existing_rules_and_rejection() {
        let bank = notetaker_core::VoiceLibrary {
            voices: vec![notetaker_core::Voice {
                id: Some("synthetic-voice".into()),
                name: "Участник А".into(),
                embeddings: vec![vec![1.0, 0.0]],
                updated_at: chrono::Utc::now(),
                is_me: false,
                sample_sources: Default::default(),
                tolk_identities: Default::default(),
            }],
            ..Default::default()
        };
        let original = bank.clone();
        let mut t = Transcript {
            segments: vec![segment("live-1", 0, 3000)],
            ..Default::default()
        };
        let embeddings = BTreeMap::from([("live-1".into(), vec![1.0, 0.0])]);
        bank.recognize(&mut t, &embeddings, true);
        assert!(t.speaker_names.is_empty());
        assert_eq!(t.voice_recognitions["live-1"].name, "Участник А");
        assert!(apply_edit(
            &mut t,
            &SpeakerEdit::RejectVoice {
                speaker: "live-1".into()
            }
        ));
        bank.recognize(&mut t, &embeddings, true);
        assert!(t.voice_recognitions.is_empty());
        assert_eq!(bank, original);
    }
    struct FinalAsr;
    impl crate::Asr for FinalAsr {
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
        fn diarize(
            &mut self,
            _: &std::path::Path,
            _: Option<u32>,
            embeddings: bool,
        ) -> Result<notetaker_asr_proto::Diarization> {
            assert!(embeddings);
            Ok(notetaker_asr_proto::Diarization {
                turns: vec![notetaker_asr_proto::SpeakerTurn {
                    start_ms: 0,
                    end_ms: 3000,
                    speaker: "system-0".into(),
                }],
                embeddings: BTreeMap::from([("system-0".into(), vec![1.0, 0.0])]),
            })
        }
    }
    #[test]
    fn final_name_transfer_uses_manual_enrollment_and_respects_disabled_library() {
        for enabled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let library = Library::open(root.path()).unwrap();
            let meeting = library
                .create_meeting(MeetingMeta::new(
                    "Синтетическая встреча",
                    chrono::Utc::now(),
                ))
                .unwrap();
            let mut clusters = Clusters::default();
            clusters.assign(0, 3000, Some(vec![1.0, 0.0]));
            let mut before = Transcript {
                segments: vec![segment("live-1", 0, 3000)],
                ..Default::default()
            };
            apply_edit(
                &mut before,
                &SpeakerEdit::Rename {
                    speaker: "live-1".into(),
                    name: "Участник А".into(),
                    is_me: false,
                    everywhere: false,
                },
            );
            meeting.write_transcript(&before).unwrap();
            let (events, _) = mpsc::channel();
            crate::diarization::final_pass(
                &meeting,
                &mut FinalAsr,
                std::path::Path::new("unused"),
                None,
                &crate::DiarizationConfig {
                    voice_library: enabled,
                    ..Default::default()
                },
                &events,
                Some((&clusters, &before)),
            );
            let result = meeting.read_transcript().unwrap();
            assert_eq!(result.speaker_name("system-0"), "Участник А");
            assert!(!result.speaker_names.contains_key("live-1"));
            let bank = library.read_voices().unwrap();
            assert_eq!(bank.voices.len(), usize::from(enabled));
            if enabled {
                assert_eq!(bank.voices[0].embeddings, vec![vec![1.0, 0.0]]);
            } else {
                assert!(
                    !library
                        .private_state_dir()
                        .join("speaker-embeddings")
                        .exists()
                );
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn stopping_during_embedding_kills_worker_and_removes_the_closed_wav() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new(
                "Синтетическая встреча",
                chrono::Utc::now(),
            ))
            .unwrap();
        std::fs::create_dir_all(meeting.path().join("audio/.work")).unwrap();
        let script = root.path().join("fake-worker.sh");
        std::fs::write(&script,"#!/bin/sh\nprintf '{\"type\":\"ready\",\"protocol_version\":1}\\n'\nread -r request\nprintf started > \"$0.started\"\nsleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = AsrConfig {
            binary: script.clone(),
            model: "auto".into(),
            provider: notetaker_asr_proto::Provider::Cpu,
            num_threads: 1,
            timeout: Duration::from_secs(30),
            vocabulary: Default::default(),
        };
        let (tx, _rx) = mpsc::channel();
        let cancel = CancellationHandle::default();
        let mut worker = Worker::start(
            config,
            meeting.clone(),
            crate::DiarizationConfig::default(),
            tx,
            cancel,
            Default::default(),
        )
        .unwrap();
        worker.enqueue(
            &segment("them", 0, 3000),
            0,
            std::sync::Arc::new(vec![0.0; 48000]),
        );
        let started = root.path().join("fake-worker.sh.started");
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let at = std::time::Instant::now();
        worker.finish();
        assert!(at.elapsed() < Duration::from_secs(3));
        assert_eq!(
            std::fs::read_dir(meeting.path().join("audio/.work"))
                .unwrap()
                .count(),
            0
        );
        assert!(!library.private_state_dir().join("voices.json").exists());
    }
}

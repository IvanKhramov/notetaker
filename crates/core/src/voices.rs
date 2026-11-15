//! Local biometric state. Never embed these vectors in portable meeting files.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Library, Meeting, Result, Transcript, storage::meeting_lock};

pub const VOICE_MODEL: &str = "eres2net-base";
pub const SUGGESTION_THRESHOLD: f32 = 0.65;
pub const SELF_THRESHOLD: f32 = 0.85;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub embeddings: Vec<Vec<f32>>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoiceLibrary {
    pub model: String,
    pub voices: Vec<Voice>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tolk_samples: BTreeSet<String>,
}
impl Default for VoiceLibrary {
    fn default() -> Self {
        Self {
            model: VOICE_MODEL.into(),
            voices: Vec::new(),
            tolk_samples: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MeetingVoices {
    pub model: String,
    pub embeddings: BTreeMap<String, Vec<f32>>,
}

pub fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(a, b)| a * b).sum();
    let norm = (a.iter().map(|v| v * v).sum::<f32>() * b.iter().map(|v| v * v).sum::<f32>()).sqrt();
    (dot.is_finite() && norm.is_finite() && norm > 1e-6).then(|| (dot / norm).clamp(-1.0, 1.0))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoiceEvidence {
    pub speaker: String,
    pub name: String,
    pub registered: bool,
    #[serde(default)]
    pub track: Option<crate::Channel>,
    pub ranges: Vec<(u64, u64)>,
}
pub struct VoiceSample {
    pub speaker: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub own: bool,
}
pub const VOICE_NAME_TAKEN: &str =
    "Голос не сохранён: имя уже занято другим голосом. Если это другой человек, уточните имя.";
fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}
fn add_sample(voice: &mut Voice, embedding: &Vec<f32>) {
    if !voice.embeddings.contains(embedding) {
        if voice.embeddings.len() >= 8 {
            voice.embeddings.remove(0);
        }
        voice.embeddings.push(embedding.clone());
        voice.updated_at = Utc::now();
    }
}
fn microphone_speaker(transcript: &Transcript, speaker: &str) -> bool {
    transcript
        .segments
        .iter()
        .any(|segment| segment.speaker.0 == speaker)
        && transcript
            .segments
            .iter()
            .filter(|segment| segment.speaker.0 == speaker)
            .all(|segment| segment.channel == crate::Channel::Mic)
}
enum VoiceKey<'a> {
    Name,
    Linked(&'a str),
    Stable(&'a str),
}
impl VoiceLibrary {
    fn identity(
        &self,
        name: &str,
        linked: Option<&str>,
        own: bool,
    ) -> std::result::Result<Option<usize>, &'static str> {
        let matches: Vec<_> = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, voice)| {
                if own {
                    voice.is_me
                } else {
                    !voice.is_me && linked.is_some_and(|id| voice.id.as_deref() == Some(id))
                }
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() > 1 {
            return Err("Собственный голос неоднозначен");
        }
        let existing = matches.first().copied();
        // Legacy duplicate names stay readable; updating a linked identity adds no duplicate.
        if existing.is_some_and(|index| same_name(&self.voices[index].name, name)) {
            return Ok(existing);
        }
        if self
            .voices
            .iter()
            .enumerate()
            .any(|(index, voice)| Some(index) != existing && same_name(&voice.name, name))
        {
            return Err(VOICE_NAME_TAKEN);
        }
        Ok(existing)
    }
    pub fn name_available(&self, name: &str, linked: Option<&str>, own: bool) -> bool {
        self.identity(name, linked, own).is_ok()
    }
    fn enroll(
        &mut self,
        key: VoiceKey<'_>,
        name: &str,
        own: bool,
        embedding: &Vec<f32>,
    ) -> Result<usize> {
        let linked = match key {
            VoiceKey::Name => None,
            VoiceKey::Linked(id) | VoiceKey::Stable(id) => Some(id),
        };
        let existing = self
            .identity(name, linked, own)
            .map_err(|reason| crate::Error::SpeakerEdit(reason.into()))?;
        let index = existing.unwrap_or_else(|| {
            let proposed = match key {
                VoiceKey::Stable(id)
                    if !own
                        && !self
                            .voices
                            .iter()
                            .any(|voice| voice.id.as_deref() == Some(id)) =>
                {
                    id.into()
                }
                _ => uuid::Uuid::now_v7().to_string(),
            };
            self.voices.push(Voice {
                id: Some(proposed),
                name: name.trim().into(),
                embeddings: Vec::new(),
                updated_at: Utc::now(),
                is_me: own,
            });
            self.voices.len() - 1
        });
        add_sample(&mut self.voices[index], embedding);
        Ok(index)
    }
    fn ensure_ids(&mut self) {
        for voice in &mut self.voices {
            if voice.id.is_none() {
                voice.id = Some(uuid::Uuid::now_v7().to_string());
            }
        }
    }

    /// Require both a threshold and a margin over the next identity.
    pub fn suggest(&self, embedding: &[f32]) -> Option<(&Voice, f32)> {
        if self.model != VOICE_MODEL {
            return None;
        }
        let mut ranked: Vec<_> = self
            .voices
            .iter()
            .filter_map(|voice| {
                voice
                    .embeddings
                    .iter()
                    .filter_map(|v| cosine(v, embedding))
                    .max_by(f32::total_cmp)
                    .map(|s| (voice, s))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (voice, score) = *ranked.first()?;
        (score >= SUGGESTION_THRESHOLD && ranked.get(1).is_none_or(|v| score - v.1 >= 0.08))
            .then_some((voice, score))
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().expect("local state parent");
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

impl Library {
    pub fn read_voices(&self) -> Result<VoiceLibrary> {
        self.relocate_private_state()?;
        match fs::read(self.private_state_dir().join("voices.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(VoiceLibrary::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Called only with the voice library enabled. Give legacy entries stable identities.
    pub fn prepare_voices(&self) -> Result<VoiceLibrary> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut voices = self.read_voices()?;
        if voices.voices.iter().any(|v| v.id.is_none()) {
            voices.ensure_ids();
            write_json(&path, &voices)?;
        }
        Ok(voices)
    }

    pub fn delete_voice(&self, name: &str) -> Result<()> {
        self.delete_voice_matching(|voice| voice.name == name)
    }
    pub fn delete_voice_id(&self, id: &str) -> Result<()> {
        self.delete_voice_matching(|voice| voice.id.as_deref() == Some(id))
    }
    fn delete_voice_matching(&self, matches: impl Fn(&Voice) -> bool) -> Result<()> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut library = self.read_voices()?;
        library.voices.retain(|voice| !matches(voice));
        write_json(&path, &library)
    }

    pub fn write_meeting_voices(
        &self,
        meeting: &Meeting,
        embeddings: BTreeMap<String, Vec<f32>>,
    ) -> Result<()> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let _files = self.lock_voice_files()?;
        let id = meeting.read_meta()?.id;
        write_json(
            &self
                .private_state_dir()
                .join(format!("speaker-embeddings/{id}.json")),
            &MeetingVoices {
                model: VOICE_MODEL.into(),
                embeddings,
            },
        )
    }

    pub fn cache_speaker_voice(
        &self,
        meeting: &Meeting,
        speaker: &str,
        embedding: Vec<f32>,
    ) -> Result<()> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let _files = self.lock_voice_files()?;
        let mut cache = self.read_meeting_voices(meeting)?;
        cache.embeddings.insert(speaker.into(), embedding);
        write_json(&self.meeting_voices_path(meeting)?, &cache)
    }
    fn lock_voice_files(&self) -> Result<Option<fs::File>> {
        fs::create_dir_all(self.private_state_dir())?;
        crate::storage::lock_meeting_files(&self.private_state_dir())
    }
    /// Discard biometric snapshots from unreleased development builds.
    pub fn discard_voice_ledgers(&self) -> Result<()> {
        let state = self.private_state_dir();
        let libraries = state.parent().expect("per-library local state");
        let entries = match fs::read_dir(libraries) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            match fs::remove_dir_all(entry.path().join("voice-enrollments")) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    /// The bank write records each meeting/identity key atomically with its sample.
    pub fn remember_tolk_voices(
        &self,
        meeting: &Meeting,
        transcript: &Transcript,
        samples: Vec<VoiceSample>,
        expected: crate::Revision,
        cancelled: impl Fn() -> bool,
    ) -> Result<(usize, Vec<String>)> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let _files = self.lock_voice_files()?;
        let lock = crate::storage::meeting_lock(meeting.path());
        let _meeting = lock.lock().unwrap_or_else(|error| error.into_inner());
        let _file = crate::storage::lock_meeting_files(meeting.path())?;
        let loaded = meeting.load_transcript()?;
        if loaded.revision != expected || loaded.value != *transcript {
            return Err(crate::Error::SpeakerEdit(
                "Транскрипт изменился после сверки, обучение пропущено".into(),
            ));
        }
        let mut voices = self.read_voices()?;
        voices.ensure_ids();
        if voices.model != VOICE_MODEL {
            return Err(crate::Error::SpeakerEdit(
                "Модель библиотеки голосов не совпадает".into(),
            ));
        }
        let mut cache = self.read_meeting_voices(meeting)?;
        let mut count = 0;
        let mut skipped = Vec::new();
        let meeting_id = meeting.read_meta()?.id;
        for sample in samples {
            let key = format!("{meeting_id}:{}", sample.speaker);
            if voices.tolk_samples.contains(&key) {
                continue;
            }
            if cosine(&sample.embedding, &sample.embedding).is_none() {
                return Err(crate::Error::SpeakerEdit(
                    "Некорректный отпечаток голоса".into(),
                ));
            }
            match voices.enroll(
                VoiceKey::Stable(&sample.speaker),
                &sample.name,
                sample.own,
                &sample.embedding,
            ) {
                Ok(_) => {
                    cache.embeddings.insert(
                        if sample.own {
                            "me".into()
                        } else {
                            sample.speaker
                        },
                        sample.embedding,
                    );
                    voices.tolk_samples.insert(key);
                    count += 1;
                }
                Err(crate::Error::SpeakerEdit(reason)) => {
                    skipped.push(format!("{}: {reason}", sample.name))
                }
                Err(error) => return Err(error),
            }
        }
        if cancelled() {
            return Err(crate::Error::SpeakerEdit("Операция Толка отменена".into()));
        }
        // On a crash between these two writes, the retry completes the bank write.
        write_json(&self.meeting_voices_path(meeting)?, &cache)?;
        write_json(&path, &voices)?;
        Ok((count, skipped))
    }
    /// Called only after explicit enrollment and a successful transcript save.
    pub fn remember_speakers(&self, meeting: &Meeting, transcript: &Transcript) -> Result<()> {
        self.remember_speakers_owned(meeting, transcript, false)
    }
    pub fn remember_speakers_owned(
        &self,
        meeting: &Meeting,
        transcript: &Transcript,
        own_allowed: bool,
    ) -> Result<()> {
        let id = meeting.read_meta()?.id;
        let bytes = match fs::read(
            self.private_state_dir()
                .join(format!("speaker-embeddings/{id}.json")),
        ) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let meeting: MeetingVoices = serde_json::from_slice(&bytes)?;
        if meeting.model != VOICE_MODEL {
            return Ok(());
        }
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut library = self.read_voices()?;
        if library.model != VOICE_MODEL {
            return Ok(());
        }
        library.ensure_ids();
        for (id, name) in &transcript.speaker_names {
            let Some(embedding) = meeting
                .embeddings
                .get(id)
                .filter(|v| cosine(v, v).is_some())
            else {
                continue;
            };
            let is_me = own_allowed && microphone_speaker(transcript, id);
            let key = transcript
                .speaker_voice_ids
                .get(id)
                .map_or(VoiceKey::Name, |id| {
                    if id.starts_with("ktalk-") {
                        VoiceKey::Stable(id)
                    } else {
                        VoiceKey::Linked(id)
                    }
                });
            library.enroll(key, name, is_me, embedding)?;
        }
        write_json(&path, &library)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_rejects_bad_vectors_and_ambiguous_identities() {
        assert_eq!(cosine(&[1.0, 0.0], &[2.0, 0.0]), Some(1.0));
        assert!(cosine(&[], &[]).is_none());
        assert!(cosine(&[0.0], &[1.0]).is_none());
        assert!(cosine(&[f32::NAN], &[1.0]).is_none());
        assert!(cosine(&[1.0], &[1.0, 0.0]).is_none());
        let mut library = VoiceLibrary::default();
        library.voices.push(Voice {
            id: None,
            name: "Саша".into(),
            embeddings: vec![vec![1.0, 0.0]],
            updated_at: Utc::now(),
            is_me: false,
        });
        assert_eq!(library.suggest(&[1.0, 0.0]).unwrap().0.name, "Саша");
        library.voices.push(Voice {
            name: "Другой".into(),
            ..library.voices[0].clone()
        });
        assert!(library.suggest(&[1.0, 0.0]).is_none());
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use crate::MeetingMeta;
    #[test]
    fn local_enrollment_roundtrip_self_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::open(dir.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Voices", Utc::now()))
            .unwrap();
        let mut text = Transcript::default();
        text.rename_speaker("s1", "Саша");
        text.rename_speaker("me", "Иван");
        text.segments.push(crate::Segment {
            id: uuid::Uuid::now_v7(),
            speaker: crate::SpeakerId("me".into()),
            channel: crate::Channel::Mic,
            start_ms: 0,
            end_ms: 7000,
            text: "Нейтральный текст".into(),
            source: crate::Source::Final,
            edited: false,
            lang: None,
        });
        library
            .write_meeting_voices(
                &meeting,
                BTreeMap::from([("s1".into(), vec![1.0, 0.0]), ("me".into(), vec![0.0, 1.0])]),
            )
            .unwrap();
        library
            .remember_speakers_owned(&meeting, &text, true)
            .unwrap();
        for voice in library.read_voices().unwrap().voices {
            let speaker = if voice.is_me { "me" } else { "s1" };
            text.speaker_voice_ids
                .insert(speaker.into(), voice.id.unwrap());
        }
        library
            .remember_speakers_owned(&meeting, &text, true)
            .unwrap();
        let saved = library.read_voices().unwrap();
        assert_eq!(saved.voices.len(), 2);
        assert!(saved.voices.iter().all(|v| v.embeddings.len() == 1));
        assert!(saved.suggest(&[0.0, 1.0]).unwrap().0.is_me);
        assert!(
            !fs::read_to_string(meeting.path().join("transcript.json"))
                .unwrap()
                .contains("embedding")
        );
        library.delete_voice("Саша").unwrap();
        assert!(
            library
                .read_voices()
                .unwrap()
                .suggest(&[1.0, 0.0])
                .is_none()
        );
        library.delete_voice("Иван").unwrap();
        assert!(library.read_voices().unwrap().voices.is_empty());
    }
}

#[derive(Clone, Debug)]
pub enum SpeakerEdit {
    Merge {
        from: String,
        into: String,
    },
    Undo(Box<SpeakerUndo>),
    Reassign {
        segments: Vec<uuid::Uuid>,
        speaker: String,
        name: Option<String>,
    },
    Rename {
        speaker: String,
        name: String,
        is_me: bool,
        everywhere: bool,
    },
}

#[derive(Clone, Debug)]
pub struct SpeakerUndo {
    transcript: crate::MergeUndo,
    // Keep both sides so undo cannot overwrite unrelated voice library edits.
    voices: Option<(MeetingVoices, MeetingVoices, VoiceLibrary, VoiceLibrary)>,
}

pub struct SpeakerEditResult {
    pub undo: Option<SpeakerUndo>,
    pub renamed_meetings: Vec<std::path::PathBuf>,
    pub voice_status: String,
}

impl Library {
    fn meeting_voices_path(&self, meeting: &Meeting) -> Result<std::path::PathBuf> {
        Ok(self.private_state_dir().join(format!(
            "speaker-embeddings/{}.json",
            meeting.read_meta()?.id
        )))
    }

    fn read_meeting_voices(&self, meeting: &Meeting) -> Result<MeetingVoices> {
        match fs::read(self.meeting_voices_path(meeting)?) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MeetingVoices {
                model: VOICE_MODEL.into(),
                ..Default::default()
            }),
            Err(error) => Err(error.into()),
        }
    }

    /// Each file is revision-checked/atomically replaced. Voice state stays local.
    /// Expected content also protects queued UI actions after a failed pending save.
    pub fn edit_speakers(
        &self,
        meeting: &Meeting,
        expected: &Transcript,
        edit: SpeakerEdit,
        voices_enabled: bool,
    ) -> Result<SpeakerEditResult> {
        self.edit_speakers_owned(meeting, expected, edit, voices_enabled, false)
    }
    pub fn edit_speakers_owned(
        &self,
        meeting: &Meeting,
        expected: &Transcript,
        edit: SpeakerEdit,
        voices_enabled: bool,
        own_mic_allowed: bool,
    ) -> Result<SpeakerEditResult> {
        let loaded = meeting.load_transcript()?;
        if &loaded.value != expected {
            return Err(crate::Error::SpeakerEdit(
                "Транскрипт изменился на диске; откройте встречу заново".into(),
            ));
        }
        let mut transcript = loaded.value;
        let voices_path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&voices_path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut voices = if voices_enabled {
            Some(self.read_voices()?)
        } else {
            None
        };
        if let Some(voices) = &mut voices {
            voices.ensure_ids();
        }
        let mut clusters = if voices_enabled {
            Some(self.read_meeting_voices(meeting)?)
        } else {
            None
        };
        let mut result = SpeakerEditResult {
            undo: None,
            renamed_meetings: Vec::new(),
            voice_status: String::new(),
        };
        let mut propagation = None;
        match edit {
            SpeakerEdit::Merge { from, into } => {
                let merge = transcript.merge_speakers(&from, &into)?;
                let mut snapshots = None;
                if let (Some(clusters), Some(voices)) = (&mut clusters, &mut voices) {
                    let old_clusters = clusters.clone();
                    let old_voices = voices.clone();
                    if clusters.model == VOICE_MODEL {
                        let source = clusters.embeddings.remove(&from);
                        if let Some(source) = source {
                            if let Some(target) = clusters.embeddings.get(&into) {
                                if cosine(target, &source).is_some() {
                                    let mut combined: Vec<_> =
                                        target.iter().zip(&source).map(|(a, b)| a + b).collect();
                                    let norm = combined.iter().map(|v| v * v).sum::<f32>().sqrt();
                                    if norm > 1e-6 {
                                        for v in &mut combined {
                                            *v /= norm;
                                        }
                                        clusters.embeddings.insert(into.clone(), combined);
                                    }
                                }
                            } else {
                                clusters.embeddings.insert(into.clone(), source.clone());
                            }
                            if voices.model == VOICE_MODEL
                                && transcript.speaker_names.contains_key(&into)
                                && let Some(voice) = voices.voices.iter_mut().find(|v| {
                                    v.id.as_ref() == transcript.speaker_voice_ids.get(&into)
                                })
                            {
                                // A merge is not an explicit own-voice confirmation.
                                if !voice.is_me {
                                    for embedding in old_clusters
                                        .embeddings
                                        .get(&into)
                                        .into_iter()
                                        .chain(std::iter::once(&source))
                                    {
                                        if cosine(embedding, embedding).is_some() {
                                            add_sample(voice, embedding);
                                        }
                                    }
                                } else {
                                    clusters.embeddings.remove(&into);
                                }
                                voice.updated_at = Utc::now();
                            }
                        }
                    }
                    snapshots = Some((old_clusters, clusters.clone(), old_voices, voices.clone()));
                }
                result.undo = Some(SpeakerUndo {
                    transcript: merge,
                    voices: snapshots,
                });
            }
            SpeakerEdit::Undo(undo) => {
                if let Some((before, after, old_voices, new_voices)) = undo.voices {
                    if !voices_enabled
                        || clusters.as_ref() != Some(&after)
                        || voices.as_ref() != Some(&new_voices)
                    {
                        return Err(crate::Error::SpeakerEdit(
                            "Библиотека голосов изменилась после объединения".into(),
                        ));
                    }
                    clusters = Some(before);
                    voices = Some(old_voices);
                }
                transcript.undo_merge(&undo.transcript)?;
            }
            SpeakerEdit::Reassign {
                segments,
                speaker,
                name,
            } => {
                transcript.reassign_segments(&segments, &speaker)?;
                if let Some(name) = name {
                    transcript.rename_speaker(&speaker, &name);
                }
                // A manually assembled cluster has no trustworthy acoustic centroid.
                // Existing voice links are retained, but this action never enrolls it.
            }
            SpeakerEdit::Rename {
                speaker,
                name,
                is_me,
                everywhere,
            } => {
                if name.trim().is_empty() || !transcript.speaker_ids().contains(&speaker) {
                    return Err(crate::Error::SpeakerEdit(
                        "Укажите имя и спикера встречи".into(),
                    ));
                }
                // Correcting an unconfirmed guess creates/selects the named identity.
                // An explicit global rename deliberately retains the matched identity.
                if !everywhere
                    && !transcript.speaker_names.contains_key(&speaker)
                    && transcript
                        .speaker_suggestions
                        .get(&speaker)
                        .is_some_and(|guess| guess != name.trim())
                {
                    transcript.speaker_voice_ids.remove(&speaker);
                }
                transcript.rename_speaker(&speaker, &name);
                if is_me {
                    transcript.self_speaker = Some(speaker.clone());
                }
                if let Some(voices) = &mut voices {
                    let own_voice =
                        is_me || own_mic_allowed && microphone_speaker(&transcript, &speaker);
                    let linked = transcript.speaker_voice_ids.get(&speaker).cloned();
                    let embedding = clusters
                        .as_ref()
                        .filter(|cache| cache.model == VOICE_MODEL)
                        .and_then(|cache| cache.embeddings.get(&speaker))
                        .filter(|embedding| cosine(embedding, embedding).is_some());
                    if voices.model == VOICE_MODEL {
                        match voices.identity(&name, linked.as_deref(), own_voice) {
                            Err(reason) => result.voice_status = reason.into(),
                            Ok(existing) => {
                                let index = if let Some(embedding) = embedding {
                                    Some(voices.enroll(
                                        linked.as_deref().map_or(VoiceKey::Name, VoiceKey::Linked),
                                        &name,
                                        own_voice,
                                        embedding,
                                    )?)
                                } else {
                                    existing
                                };
                                if let Some(index) = index {
                                    let voice = &mut voices.voices[index];
                                    if let Some(id) = &voice.id {
                                        transcript.speaker_voice_ids.insert(speaker, id.clone());
                                        if everywhere {
                                            voice.name = name.trim().into();
                                            propagation =
                                                Some((id.clone(), name.trim().to_owned()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        meeting.save_transcript(&transcript, loaded.revision)?;
        if let Some(clusters) = clusters {
            write_json(&self.meeting_voices_path(meeting)?, &clusters)?;
        }
        if let Some(voices) = voices {
            write_json(&voices_path, &voices)?;
        }
        if let Some((voice_id, name)) = propagation {
            for previous in self.list_meetings()? {
                let mut loaded = previous.load_transcript()?;
                let ids: Vec<_> = loaded
                    .value
                    .speaker_voice_ids
                    .iter()
                    .filter(|(_, id)| **id == voice_id)
                    .map(|(speaker, _)| speaker.clone())
                    .collect();
                let mut changed = false;
                for id in ids {
                    if loaded.value.speaker_names.contains_key(&id) {
                        changed |= loaded.value.rename_speaker(&id, &name);
                    } else if let Some(suggestion) = loaded.value.speaker_suggestions.get_mut(&id)
                        && *suggestion != name
                    {
                        suggestion.clone_from(&name);
                        changed = true;
                    }
                }
                if changed {
                    previous.save_transcript(&loaded.value, loaded.revision)?;
                    result.renamed_meetings.push(previous.path().to_owned());
                }
            }
        }
        self.reindex_incremental()?;
        Ok(result)
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::{Channel, MeetingMeta, Segment, Source, SpeakerId};
    #[test]
    fn unique_names_and_own_identity_never_promote_a_namesake() {
        let mut bank = VoiceLibrary::default();
        bank.enroll(
            VoiceKey::Stable("namesake"),
            "Олег Сидоров",
            false,
            &vec![1.0, 0.0],
        )
        .unwrap();
        let before = bank.clone();
        assert!(
            bank.enroll(VoiceKey::Name, "  ОЛЕГ СИДОРОВ ", true, &vec![0.0, 1.0])
                .is_err()
        );
        assert_eq!(bank, before);
        bank.enroll(VoiceKey::Name, "Олег, мой голос", true, &vec![0.0, 1.0])
            .unwrap();
        assert_eq!(bank.voices.iter().filter(|voice| voice.is_me).count(), 1);
        assert!(!bank.voices[0].is_me);
        let own = bank.voices[1].clone();
        for key in [
            VoiceKey::Name,
            VoiceKey::Stable("ktalk-guest"),
            VoiceKey::Stable("other-registered"),
        ] {
            assert!(
                bank.enroll(key, "Олег, мой голос", false, &vec![1.0, 1.0])
                    .is_err()
            );
        }
        assert_eq!(bank.voices[1], own);
        assert!(
            bank.enroll(
                VoiceKey::Stable("second-namesake"),
                "Олег Сидоров",
                false,
                &vec![0.5, 0.5]
            )
            .is_err()
        );
        bank.enroll(
            VoiceKey::Linked("namesake"),
            "Олег Сидоров",
            false,
            &vec![0.5, 0.5],
        )
        .unwrap();
        assert_eq!(bank.voices.len(), 2);
        assert_eq!(bank.voices[0].embeddings.len(), 2);
    }
    #[test]
    fn legacy_duplicates_remain_readable_without_creating_more() {
        let mut bank = VoiceLibrary::default();
        bank.enroll(
            VoiceKey::Stable("first"),
            "Участник",
            false,
            &vec![1.0, 0.0],
        )
        .unwrap();
        let mut duplicate = bank.voices[0].clone();
        duplicate.id = Some("legacy".into());
        bank.voices.push(duplicate);
        bank.enroll(
            VoiceKey::Linked("first"),
            "Участник",
            false,
            &vec![0.0, 1.0],
        )
        .unwrap();
        assert!(
            bank.enroll(VoiceKey::Name, "УЧАСТНИК", false, &vec![1.0, 1.0])
                .is_err()
        );
        assert_eq!(bank.voices.len(), 2);
    }
    fn fixture() -> (tempfile::TempDir, Library, Meeting, Transcript) {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let meeting = library
            .create_meeting(MeetingMeta::new("Пример", Utc::now()))
            .unwrap();
        let mut text = Transcript::default();
        for id in ["s1", "ktalk-guest", "me"] {
            text.segments.push(Segment {
                id: uuid::Uuid::now_v7(),
                speaker: SpeakerId(id.into()),
                channel: if id == "me" {
                    Channel::Mic
                } else {
                    Channel::System
                },
                start_ms: 0,
                end_ms: 7000,
                text: "Нейтральный текст".into(),
                source: Source::Final,
                edited: false,
                lang: None,
            });
        }
        meeting.write_transcript(&text).unwrap();
        library
            .write_meeting_voices(
                &meeting,
                BTreeMap::from([
                    ("s1".into(), vec![1.0, 0.0]),
                    ("ktalk-guest".into(), vec![1.0, 1.0]),
                    ("me".into(), vec![0.0, 1.0]),
                ]),
            )
            .unwrap();
        (root, library, meeting, text)
    }
    #[test]
    fn manual_guest_and_microphone_collisions_keep_names_but_do_not_save_voices() {
        let (_root, library, meeting, mut text) = fixture();
        for (speaker, own) in [("s1", false), ("me", true), ("ktalk-guest", false)] {
            let result = library
                .edit_speakers_owned(
                    &meeting,
                    &text,
                    SpeakerEdit::Rename {
                        speaker: speaker.into(),
                        name: "Олег Сидоров".into(),
                        is_me: false,
                        everywhere: false,
                    },
                    true,
                    own,
                )
                .unwrap();
            text = meeting.read_transcript().unwrap();
            assert_eq!(text.speaker_names[speaker], "Олег Сидоров");
            if speaker != "s1" {
                assert_eq!(result.voice_status, VOICE_NAME_TAKEN);
            }
        }
        let bank = library.read_voices().unwrap();
        assert_eq!(bank.voices.len(), 1);
        assert!(!bank.voices[0].is_me);
        assert_eq!(bank.voices[0].embeddings, vec![vec![1.0, 0.0]]);
    }
    #[test]
    fn restarted_enrollment_is_idempotent_and_creates_no_ledger() {
        let (root, library, meeting, text) = fixture();
        let revision = meeting.load_transcript().unwrap().revision;
        for vector in [vec![1.0, 0.0], vec![0.0, 1.0]] {
            let reopened = Library::open(root.path()).unwrap();
            reopened
                .remember_tolk_voices(
                    &meeting,
                    &text,
                    vec![VoiceSample {
                        speaker: "ktalk-user".into(),
                        name: "Анна Петрова".into(),
                        embedding: vector,
                        own: false,
                    }],
                    revision,
                    || false,
                )
                .unwrap();
        }
        let bank = library.read_voices().unwrap();
        assert_eq!(bank.voices.len(), 1);
        assert_eq!(bank.voices[0].embeddings, vec![vec![1.0, 0.0]]);
        assert_eq!(bank.tolk_samples.len(), 1);
        assert!(
            !library
                .private_state_dir()
                .join("voice-enrollments")
                .exists()
        );
        let allowed = ["voices.json", "speaker-embeddings", ".notetaker.lock"];
        for file in fs::read_dir(library.private_state_dir()).unwrap() {
            assert!(allowed.contains(&file.unwrap().file_name().to_str().unwrap()));
        }
    }
}

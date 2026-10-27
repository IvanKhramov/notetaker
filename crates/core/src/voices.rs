//! Local biometric state. Never embed these vectors in portable meeting files.
use std::{collections::BTreeMap, fs, io::Write, path::Path};

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
}
impl Default for VoiceLibrary {
    fn default() -> Self {
        Self {
            model: VOICE_MODEL.into(),
            voices: Vec::new(),
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

impl VoiceLibrary {
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
        match fs::read(self.root.join(".notetaker/voices.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(VoiceLibrary::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Called only with the voice library enabled. Give legacy entries stable identities.
    pub fn prepare_voices(&self) -> Result<VoiceLibrary> {
        let path = self.root.join(".notetaker/voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut voices = self.read_voices()?;
        if voices.voices.iter().any(|v| v.id.is_none()) {
            voices.ensure_ids();
            write_json(&path, &voices)?;
        }
        Ok(voices)
    }

    pub fn delete_voice(&self, name: &str) -> Result<()> {
        let path = self.root.join(".notetaker/voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut library = self.read_voices()?;
        library.voices.retain(|v| v.name != name);
        write_json(&path, &library)
    }

    pub fn write_meeting_voices(
        &self,
        meeting: &Meeting,
        embeddings: BTreeMap<String, Vec<f32>>,
    ) -> Result<()> {
        let id = meeting.read_meta()?.id;
        write_json(
            &self
                .root
                .join(format!(".notetaker/speaker-embeddings/{id}.json")),
            &MeetingVoices {
                model: VOICE_MODEL.into(),
                embeddings,
            },
        )
    }

    /// Called only after explicit enrollment and a successful transcript save.
    pub fn remember_speakers(&self, meeting: &Meeting, transcript: &Transcript) -> Result<()> {
        let id = meeting.read_meta()?.id;
        let bytes = match fs::read(
            self.root
                .join(format!(".notetaker/speaker-embeddings/{id}.json")),
        ) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let meeting: MeetingVoices = serde_json::from_slice(&bytes)?;
        if meeting.model != VOICE_MODEL {
            return Ok(());
        }
        let path = self.root.join(".notetaker/voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
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
            let is_me = id == "me" || transcript.self_speaker.as_ref() == Some(id);
            let existing = library.voices.iter().position(|v| {
                transcript
                    .speaker_voice_ids
                    .get(id)
                    .map_or(v.name == *name, |linked| v.id.as_ref() == Some(linked))
            });
            let index = existing.unwrap_or_else(|| {
                library.voices.push(Voice {
                    id: Some(uuid::Uuid::now_v7().to_string()),
                    name: name.clone(),
                    embeddings: Vec::new(),
                    updated_at: Utc::now(),
                    is_me,
                });
                library.voices.len() - 1
            });
            if is_me {
                for voice in &mut library.voices {
                    voice.is_me = false;
                }
            }
            let voice = &mut library.voices[index];
            voice.is_me |= is_me;
            if !voice.embeddings.contains(embedding) {
                if voice.embeddings.len() >= 8 {
                    voice.embeddings.remove(0);
                }
                voice.embeddings.push(embedding.clone());
                voice.updated_at = Utc::now();
            }
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
        library
            .write_meeting_voices(
                &meeting,
                BTreeMap::from([("s1".into(), vec![1.0, 0.0]), ("me".into(), vec![0.0, 1.0])]),
            )
            .unwrap();
        library.remember_speakers(&meeting, &text).unwrap();
        library.remember_speakers(&meeting, &text).unwrap();
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
}

impl Library {
    fn meeting_voices_path(&self, meeting: &Meeting) -> Result<std::path::PathBuf> {
        Ok(self.root.join(format!(
            ".notetaker/speaker-embeddings/{}.json",
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
        let loaded = meeting.load_transcript()?;
        if &loaded.value != expected {
            return Err(crate::Error::SpeakerEdit(
                "Транскрипт изменился на диске; откройте встречу заново".into(),
            ));
        }
        let mut transcript = loaded.value;
        let voices_path = self.root.join(".notetaker/voices.json");
        let lock = meeting_lock(&voices_path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
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
                            let is_me =
                                into == "me" || transcript.self_speaker.as_ref() == Some(&into);
                            if is_me && voices.model == VOICE_MODEL {
                                for voice in &mut voices.voices {
                                    voice.is_me = false;
                                }
                            }
                            if voices.model == VOICE_MODEL
                                && transcript.speaker_names.contains_key(&into)
                                && let Some(voice) = voices.voices.iter_mut().find(|v| {
                                    v.id.as_ref() == transcript.speaker_voice_ids.get(&into)
                                })
                            {
                                for embedding in old_clusters
                                    .embeddings
                                    .get(&into)
                                    .into_iter()
                                    .chain(std::iter::once(&source))
                                {
                                    if cosine(embedding, embedding).is_some()
                                        && !voice.embeddings.contains(embedding)
                                    {
                                        if voice.embeddings.len() >= 8 {
                                            voice.embeddings.remove(0);
                                        }
                                        voice.embeddings.push(embedding.clone());
                                    }
                                }
                                voice.is_me |= is_me;
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
                    let linked = transcript.speaker_voice_ids.get(&speaker).cloned();
                    let existing = voices.voices.iter().position(|v| {
                        linked
                            .as_ref()
                            .map_or(v.name == name.trim(), |id| v.id.as_ref() == Some(id))
                    });
                    let embedding = clusters
                        .as_ref()
                        .filter(|v| v.model == VOICE_MODEL)
                        .and_then(|v| v.embeddings.get(&speaker))
                        .filter(|v| cosine(v, v).is_some());
                    if voices.model == VOICE_MODEL && (existing.is_some() || embedding.is_some()) {
                        let index = existing.unwrap_or_else(|| {
                            voices.voices.push(Voice {
                                id: Some(uuid::Uuid::now_v7().to_string()),
                                name: name.trim().into(),
                                embeddings: Vec::new(),
                                updated_at: Utc::now(),
                                is_me: false,
                            });
                            voices.voices.len() - 1
                        });
                        let own_voice = is_me
                            || speaker == "me"
                            || transcript.self_speaker.as_ref() == Some(&speaker);
                        if own_voice {
                            for voice in &mut voices.voices {
                                voice.is_me = false;
                            }
                        }
                        let voice = &mut voices.voices[index];
                        if let Some(embedding) = embedding.filter(|e| !voice.embeddings.contains(e))
                        {
                            if voice.embeddings.len() >= 8 {
                                voice.embeddings.remove(0);
                            }
                            voice.embeddings.push(embedding.clone());
                        }
                        voice.is_me |= own_voice;
                        voice.updated_at = Utc::now();
                        let voice_id = voice.id.clone().expect("prepared identity");
                        transcript
                            .speaker_voice_ids
                            .insert(speaker, voice_id.clone());
                        if everywhere {
                            voice.name = name.trim().into();
                            propagation = Some((voice_id, name.trim().to_owned()));
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

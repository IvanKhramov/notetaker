//! Local biometric state. Never embed these vectors in portable meeting files.
use std::{collections::BTreeMap, fs, io::Write, path::Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{Library, Meeting, Result, Transcript, storage::meeting_lock};

pub const VOICE_MODEL: &str = "eres2net-base";
pub const SUGGESTION_THRESHOLD: f32 = 0.65;
pub const SELF_THRESHOLD: f32 = 0.85;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Voice {
    pub name: String,
    pub embeddings: Vec<Vec<f32>>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub is_me: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Default, Serialize, Deserialize)]
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
        for (id, name) in &transcript.speaker_names {
            let Some(embedding) = meeting
                .embeddings
                .get(id)
                .filter(|v| cosine(v, v).is_some())
            else {
                continue;
            };
            let is_me = id == "me" || transcript.self_speaker.as_ref() == Some(id);
            let existing = library.voices.iter().position(|v| v.name == *name);
            let index = existing.unwrap_or_else(|| {
                library.voices.push(Voice {
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

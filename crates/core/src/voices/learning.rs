use super::*;
use sha2::{Digest, Sha256};
fn fingerprint(embedding: &[f32]) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            embedding
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>()
        )
    )
}
pub(super) fn add_diverse_sample(voice: &mut Voice, embedding: &Vec<f32>, source: Option<&str>) {
    if voice.embeddings.contains(embedding) {
        return;
    }
    if let Some(source) = source
        && let Some(index) = voice.embeddings.iter().position(|v| {
            voice
                .sample_sources
                .get(&fingerprint(v))
                .is_some_and(|old| old == source)
        })
    {
        let old = voice.embeddings.remove(index);
        voice.sample_sources.remove(&fingerprint(&old));
    }
    if voice.embeddings.len() >= 8 {
        let index = voice
            .embeddings
            .iter()
            .position(|v| !voice.sample_sources.contains_key(&fingerprint(v)))
            .unwrap_or(0);
        let old = voice.embeddings.remove(index);
        voice.sample_sources.remove(&fingerprint(&old));
    }
    voice.embeddings.push(embedding.clone());
    if let Some(source) = source {
        voice
            .sample_sources
            .insert(fingerprint(embedding), source.into());
    }
    voice.updated_at = Utc::now();
}
impl Library {
    pub fn voice_learning_path(&self) -> std::path::PathBuf {
        self.private_state_dir().join("tolk-learning.json")
    }
    pub fn save_voice_learning_state(&self, value: &impl Serialize) -> Result<()> {
        write_json(&self.voice_learning_path(), value)
    }
    /// The source key and sample commit together in voices.json, including on restart.
    pub fn learn_recording_voices(
        &self,
        source: &str,
        samples: Vec<VoiceSample>,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<String>> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut bank = self.read_voices()?;
        let before = bank.clone();
        bank.ensure_ids();
        if bank.model != VOICE_MODEL {
            return Err(crate::Error::SpeakerEdit(
                "Модель библиотеки голосов не совпадает".into(),
            ));
        }
        let mut learned = Vec::new();
        for sample in samples {
            let key = format!("recording:{source}:{}", sample.speaker);
            if bank.tolk_samples.contains(&key) {
                continue;
            }
            if cosine(&sample.embedding, &sample.embedding).is_none() {
                return Err(crate::Error::SpeakerEdit(
                    "Некорректный отпечаток голоса".into(),
                ));
            }
            bank.enroll_evidence(
                VoiceKey::Stable(&sample.speaker),
                &sample.name,
                sample.own,
                &sample.embedding,
                sample.recording_id.as_deref(),
            )?;
            bank.tolk_samples.insert(key);
            learned.push(sample.speaker);
        }
        if cancelled() {
            return Err(crate::Error::SpeakerEdit(
                "Обучение голосов отменено".into(),
            ));
        }
        if bank != before {
            write_json(&path, &bank)?;
        }
        Ok(learned)
    }
    pub fn learned_recording_identity(&self, source: &str, identity: &str) -> Result<bool> {
        Ok(self
            .read_voices()?
            .tolk_samples
            .contains(&format!("recording:{source}:{identity}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_prefer_distinct_recordings_and_pair_commits_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        for n in 0..12 {
            let source = format!("recording-{n}");
            let sample = VoiceSample {
                recording_id: Some(source.clone()),
                speaker: "ktalk-a".into(),
                name: "Анна Петрова".into(),
                embedding: vec![1.0, n as f32 / 100.0],
                own: false,
            };
            lib.learn_recording_voices(&source, vec![sample], || false)
                .unwrap();
        }
        let bank = lib.read_voices().unwrap();
        assert_eq!(bank.voices[0].embeddings.len(), 8);
        assert_eq!(
            bank.voices[0]
                .sample_sources
                .values()
                .collect::<BTreeSet<_>>()
                .len(),
            8
        );
        let before = bank.clone();
        let reopened = Library::open(dir.path()).unwrap();
        assert!(
            reopened
                .learned_recording_identity("recording-11", "ktalk-a")
                .unwrap()
        );
        assert!(
            reopened
                .learn_recording_voices(
                    "recording-11",
                    vec![VoiceSample {
                        recording_id: Some("recording-11".into()),
                        speaker: "ktalk-a".into(),
                        name: "Анна Петрова".into(),
                        embedding: vec![0.9, 0.1],
                        own: false
                    }],
                    || false
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(reopened.read_voices().unwrap(), before);
        let failed = reopened.learn_recording_voices(
            "cancelled",
            vec![VoiceSample {
                recording_id: Some("cancelled".into()),
                speaker: "ktalk-b".into(),
                name: "Oleg Sidorov".into(),
                embedding: vec![0.0, 1.0],
                own: false,
            }],
            || true,
        );
        assert!(failed.is_err());
        assert_eq!(reopened.read_voices().unwrap(), before);
    }
    #[test]
    fn another_key_owner_cannot_pollute_an_existing_own_identity() {
        let mut bank = VoiceLibrary::default();
        bank.enroll_evidence(
            VoiceKey::Stable("ktalk-owner-a"),
            "Oleg Sidorov",
            true,
            &vec![1.0, 0.0],
            Some("a"),
        )
        .unwrap();
        let before = bank.clone();
        assert!(
            bank.enroll_evidence(
                VoiceKey::Stable("ktalk-owner-b"),
                "Анна Петрова",
                true,
                &vec![0.0, 1.0],
                Some("b")
            )
            .is_err()
        );
        assert_eq!(bank, before);
    }
}

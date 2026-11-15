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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sample_origins: BTreeMap<String, BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub identity_revision: u64,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub origins: BTreeMap<String, String>,
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
fn is_zero(value: &u64) -> bool {
    *value == 0
}
fn sample_key(value: &[f32]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for number in value {
        hash.update(if *number == 0.0 {
            0_f32.to_le_bytes()
        } else {
            number.to_le_bytes()
        });
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn manual_source(meeting: &Meeting) -> Result<String> {
    Ok(format!(
        "manual:{}:{}",
        meeting.read_meta()?.id,
        uuid::Uuid::now_v7()
    ))
}
fn sync_source(id: VoiceEnrollmentId) -> String {
    format!("sync:{}:{}:{}", id.meeting, id.revision.key(), id.unit)
}
fn ensure_origins(voice: &mut Voice) {
    for sample in &voice.embeddings {
        voice
            .sample_origins
            .entry(sample_key(sample))
            .or_insert_with(|| BTreeSet::from(["legacy".into()]));
    }
}
fn add_sample(voice: &mut Voice, embedding: &Vec<f32>, source: &str) {
    ensure_origins(voice);
    if !voice.embeddings.contains(embedding) {
        if voice.embeddings.len() >= 8 {
            let old = voice.embeddings.remove(0);
            voice.sample_origins.remove(&sample_key(&old));
        }
        voice.embeddings.push(embedding.clone());
        voice.updated_at = Utc::now();
    }
    voice
        .sample_origins
        .entry(sample_key(embedding))
        .or_default()
        .insert(source.into());
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceEnrollmentId {
    pub meeting: uuid::Uuid,
    pub revision: crate::Revision,
    pub unit: uuid::Uuid,
}
impl VoiceEnrollmentId {
    pub fn new(meeting: uuid::Uuid, revision: crate::Revision) -> Self {
        Self {
            meeting,
            revision,
            unit: uuid::Uuid::now_v7(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct EvictedSample {
    value: Vec<f32>,
    origins: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct VoiceEnrollment {
    id: VoiceEnrollmentId,
    #[serde(default)]
    undone: bool,
    source: String,
    before: VoiceLibrary,
    after: VoiceLibrary,
    cache_before: MeetingVoices,
    cache_after: MeetingVoices,
    #[serde(default)]
    identities: BTreeMap<String, (String, bool, u64)>,
    #[serde(default)]
    evicted: BTreeMap<String, Vec<EvictedSample>>,
}
enum VoiceKey<'a> {
    Name,
    Linked(&'a str),
    Stable(&'a str),
}
impl VoiceLibrary {
    fn enroll(
        &mut self,
        key: VoiceKey<'_>,
        name: &str,
        is_me: bool,
        embedding: &Vec<f32>,
        source: &str,
    ) {
        let existing = self.voices.iter().position(|voice| {
            (is_me || !voice.is_me)
                && match key {
                    VoiceKey::Name => voice.name == name,
                    VoiceKey::Linked(id) | VoiceKey::Stable(id) => voice.id.as_deref() == Some(id),
                }
        });
        let index = existing.unwrap_or_else(|| {
            let proposed = match key {
                VoiceKey::Stable(id)
                    if !self
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
                name: name.into(),
                embeddings: Vec::new(),
                updated_at: Utc::now(),
                is_me,
                sample_origins: Default::default(),
                identity_revision: 0,
            });
            self.voices.len() - 1
        });
        if is_me {
            for voice in &mut self.voices {
                voice.is_me = false;
            }
        }
        let voice = &mut self.voices[index];
        voice.is_me |= is_me;
        add_sample(voice, embedding, source);
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
                origins: embeddings
                    .keys()
                    .map(|key| (key.clone(), format!("local:{}", uuid::Uuid::now_v7())))
                    .collect(),
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
        cache
            .origins
            .insert(speaker.into(), manual_source(meeting)?);
        write_json(&self.meeting_voices_path(meeting)?, &cache)
    }
    fn lock_voice_files(&self) -> Result<Option<fs::File>> {
        fs::create_dir_all(self.private_state_dir())?;
        crate::storage::lock_meeting_files(&self.private_state_dir())
    }
    fn voice_enrollment_path(&self, id: VoiceEnrollmentId) -> std::path::PathBuf {
        self.private_state_dir()
            .join(format!("voice-enrollments/{}/{}.json", id.meeting, id.unit))
    }
    pub fn tolk_enrollment_active(&self, id: VoiceEnrollmentId) -> Result<bool> {
        match fs::read(self.voice_enrollment_path(id)) {
            Ok(bytes) => Ok(!serde_json::from_slice::<VoiceEnrollment>(&bytes)?.undone),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
    }
    /// Revert only this sync's contribution. A changed identity requires manual review.
    pub fn undo_tolk_voices(
        &self,
        meeting: &Meeting,
        enrollment: VoiceEnrollmentId,
    ) -> Result<Vec<String>> {
        let ledger_path = self.voice_enrollment_path(enrollment);
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut ledger: VoiceEnrollment = match fs::read(&ledger_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let revoked = VoiceEnrollment {
                    id: enrollment,
                    undone: true,
                    source: sync_source(enrollment),
                    before: VoiceLibrary::default(),
                    after: VoiceLibrary::default(),
                    cache_before: MeetingVoices::default(),
                    cache_after: MeetingVoices::default(),
                    identities: Default::default(),
                    evicted: Default::default(),
                };
                write_json(&ledger_path, &revoked)?;
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        // Block any in-flight enrollment for this unit before removing its samples.
        ledger.undone = true;
        write_json(&ledger_path, &ledger)?;
        let mut voices = self.read_voices()?;
        let mut review = BTreeSet::new();
        for after in ledger.after.voices.iter().filter(|voice| {
            voice
                .sample_origins
                .values()
                .any(|origins| origins.contains(&ledger.source))
        }) {
            let Some(id) = &after.id else { continue };
            let Some(index) = voices
                .voices
                .iter()
                .position(|voice| voice.id.as_ref() == Some(id))
            else {
                continue;
            };
            let current = &voices.voices[index];
            let identity = ledger.identities.get(id);
            if voices.model != ledger.after.model
                || identity
                    != Some(&(
                        current.name.clone(),
                        current.is_me,
                        current.identity_revision,
                    ))
            {
                review.insert(format!("{} [{id}]", current.name));
                continue;
            }
            let before = ledger
                .before
                .voices
                .iter()
                .find(|voice| voice.id.as_ref() == Some(id));
            let added: Vec<_> = after
                .embeddings
                .iter()
                .filter(|sample| {
                    after
                        .sample_origins
                        .get(&sample_key(sample))
                        .is_some_and(|origins| origins.contains(&ledger.source))
                })
                .filter(|sample| before.is_none_or(|voice| !voice.embeddings.contains(sample)))
                .collect();
            if added.iter().any(|sample| {
                current.embeddings.contains(sample)
                    && !current
                        .sample_origins
                        .get(&sample_key(sample))
                        .is_some_and(|origins| origins.contains(&ledger.source))
            }) {
                review.insert(format!("{} [{id}]", current.name));
                continue;
            }
            let mut current = current.clone();
            for sample in &current.embeddings {
                let key = sample_key(sample);
                if let Some(origins) = current.sample_origins.get_mut(&key) {
                    origins.remove(&ledger.source);
                }
            }
            current.embeddings.retain(|sample| {
                !added.contains(&sample)
                    || current
                        .sample_origins
                        .get(&sample_key(sample))
                        .is_some_and(|origins| !origins.is_empty())
            });
            current.sample_origins.retain(|key, origins| {
                !origins.is_empty()
                    && current
                        .embeddings
                        .iter()
                        .any(|sample| sample_key(sample) == *key)
            });
            if let Some(evicted) = ledger.evicted.get(id) {
                for sample in evicted.iter().rev() {
                    let mut origins = sample.origins.clone();
                    origins.remove(&ledger.source);
                    if !origins.is_empty() && !current.embeddings.contains(&sample.value) {
                        if current.embeddings.len() < 8 {
                            current.embeddings.insert(0, sample.value.clone());
                            current
                                .sample_origins
                                .insert(sample_key(&sample.value), origins);
                        } else {
                            review.insert(format!("{} [{id}]", current.name));
                        }
                    }
                }
            }
            if let Some(before) = before {
                let mut normalized_before = before.clone();
                ensure_origins(&mut normalized_before);
                let mut normalized_current = current.clone();
                ensure_origins(&mut normalized_current);
                if current.embeddings == before.embeddings
                    && normalized_before.sample_origins == normalized_current.sample_origins
                {
                    current = before.clone();
                }
            }
            if before.is_none() && current.embeddings.is_empty() {
                voices.voices.remove(index);
            } else {
                voices.voices[index] = current;
            }
        }
        let mut cache = self.read_meeting_voices(meeting)?;
        for (id, origin) in &ledger.cache_after.origins {
            if origin != &ledger.source {
                continue;
            }
            if cache.origins.get(id) != Some(origin)
                || cache.embeddings.get(id) != ledger.cache_after.embeddings.get(id)
            {
                if cache.embeddings.contains_key(id)
                    && cache.origins.get(id).is_none_or(|origin| {
                        !origin.starts_with("manual:") && !origin.starts_with("local:")
                    })
                {
                    review.insert(format!("Кэш: {id}"));
                }
                continue;
            }
            if let Some(before) = ledger.cache_before.embeddings.get(id) {
                cache.embeddings.insert(id.clone(), before.clone());
            } else {
                cache.embeddings.remove(id);
            }
            if let Some(origin) = ledger.cache_before.origins.get(id) {
                cache.origins.insert(id.clone(), origin.clone());
            } else {
                cache.origins.remove(id);
            }
        }
        write_json(&path, &voices)?;
        write_json(&self.meeting_voices_path(meeting)?, &cache)?;
        ledger.evicted.clear();
        ledger.before.voices.clear();
        ledger.after.voices.clear();
        ledger.cache_before.embeddings.clear();
        ledger.cache_after.embeddings.clear();
        ledger.cache_before.origins.clear();
        ledger.cache_after.origins.clear();
        write_json(&ledger_path, &ledger)?;
        Ok(review.into_iter().collect())
    }
    /// Tolk identities never fall back to a display-name lookup.
    pub fn remember_tolk_voices(
        &self,
        meeting: &Meeting,
        transcript: &Transcript,
        samples: Vec<VoiceSample>,
        enrollment: VoiceEnrollmentId,
        cancelled: impl Fn() -> bool,
    ) -> Result<usize> {
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let _files = self.lock_voice_files()?;
        if cancelled() {
            return Err(crate::Error::SpeakerEdit("Операция Толка отменена".into()));
        }
        let _meeting_guard = crate::storage::meeting_lock(meeting.path());
        let _meeting_guard = _meeting_guard.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = crate::storage::lock_meeting_files(meeting.path())?;
        let loaded = meeting.load_transcript()?;
        if meeting.read_meta()?.id != enrollment.meeting
            || loaded.revision != enrollment.revision
            || loaded.value != *transcript
        {
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
        let mut clusters = self.read_meeting_voices(meeting)?;
        let source = sync_source(enrollment);
        let ledger_path = self.voice_enrollment_path(enrollment);
        let mut ledger = match fs::read(&ledger_path) {
            Ok(bytes) => serde_json::from_slice::<VoiceEnrollment>(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => VoiceEnrollment {
                id: enrollment,
                undone: false,
                source: source.clone(),
                before: voices.clone(),
                after: voices.clone(),
                cache_before: clusters.clone(),
                cache_after: clusters.clone(),
                identities: Default::default(),
                evicted: Default::default(),
            },
            Err(error) => return Err(error.into()),
        };
        if ledger.undone {
            return Err(crate::Error::SpeakerEdit(
                "Сверка отменена; обучение пропущено".into(),
            ));
        }
        let mut count = 0;
        for sample in samples {
            if cosine(&sample.embedding, &sample.embedding).is_none() {
                return Err(crate::Error::SpeakerEdit(
                    "Некорректный отпечаток голоса".into(),
                ));
            }
            let id = if sample.own {
                let own: Vec<_> = voices.voices.iter().filter(|voice| voice.is_me).collect();
                if own.len() > 1 {
                    return Err(crate::Error::SpeakerEdit(
                        "Собственный голос неоднозначен".into(),
                    ));
                }
                own.first()
                    .and_then(|voice| voice.id.clone())
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
            } else {
                if voices
                    .voices
                    .iter()
                    .any(|voice| voice.id.as_deref() == Some(&sample.speaker) && voice.is_me)
                {
                    continue;
                }
                sample.speaker.clone()
            };
            let previous = voices
                .voices
                .iter()
                .find(|voice| voice.id.as_deref() == Some(&id))
                .cloned()
                .map(|mut voice| {
                    ensure_origins(&mut voice);
                    voice
                });
            voices.enroll(
                VoiceKey::Stable(&id),
                &sample.name,
                sample.own,
                &sample.embedding,
                &source,
            );
            if let Some(previous) = previous
                && let Some(current) = voices
                    .voices
                    .iter()
                    .find(|voice| voice.id.as_deref() == Some(&id))
            {
                for old in previous
                    .embeddings
                    .iter()
                    .filter(|sample| !current.embeddings.contains(sample))
                {
                    let evicted = ledger.evicted.entry(id.clone()).or_default();
                    if !evicted.iter().any(|sample| sample.value == *old) {
                        evicted.push(EvictedSample {
                            value: old.clone(),
                            origins: previous
                                .sample_origins
                                .get(&sample_key(old))
                                .cloned()
                                .unwrap_or_default(),
                        });
                    }
                }
            }
            if clusters.origins.get(&sample.speaker) != Some(&source) {
                if let Some(value) = clusters.embeddings.get(&sample.speaker) {
                    ledger
                        .cache_before
                        .embeddings
                        .insert(sample.speaker.clone(), value.clone());
                } else {
                    ledger.cache_before.embeddings.remove(&sample.speaker);
                }
                if let Some(origin) = clusters.origins.get(&sample.speaker) {
                    ledger
                        .cache_before
                        .origins
                        .insert(sample.speaker.clone(), origin.clone());
                } else {
                    ledger.cache_before.origins.remove(&sample.speaker);
                }
            }
            clusters
                .origins
                .insert(sample.speaker.clone(), source.clone());
            clusters.embeddings.insert(sample.speaker, sample.embedding);
            count += 1;
        }
        if cancelled() {
            return Err(crate::Error::SpeakerEdit("Операция Толка отменена".into()));
        }
        for voice in &voices.voices {
            if voice
                .sample_origins
                .values()
                .any(|origins| origins.contains(&source))
                && let Some(id) = &voice.id
            {
                ledger
                    .identities
                    .entry(id.clone())
                    .or_insert_with(|| (voice.name.clone(), voice.is_me, voice.identity_revision));
            }
        }
        ledger.after = voices.clone();
        ledger.cache_after = clusters.clone();
        write_json(&ledger_path, &ledger)?;
        write_json(&self.meeting_voices_path(meeting)?, &clusters)?;
        write_json(&path, &voices)?;
        Ok(count)
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
        let source = manual_source(meeting)?;
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
            let is_me = own_allowed && (id == "me" || transcript.self_speaker.as_ref() == Some(id));
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
            library.enroll(key, name, is_me, embedding, &source);
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
            sample_origins: Default::default(),
            identity_revision: 0,
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
        library
            .remember_speakers_owned(&meeting, &text, true)
            .unwrap();
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
        let sample_source = manual_source(meeting)?;
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
                                            add_sample(voice, embedding, &sample_source);
                                        }
                                    }
                                } else {
                                    clusters.embeddings.remove(&into);
                                }
                                voice.identity_revision = voice.identity_revision.saturating_add(1);
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
                    let own_voice = is_me
                        || own_mic_allowed
                            && transcript
                                .segments
                                .iter()
                                .any(|segment| segment.speaker.0 == speaker)
                            && transcript
                                .segments
                                .iter()
                                .filter(|segment| segment.speaker.0 == speaker)
                                .all(|segment| segment.channel == crate::Channel::Mic);
                    let linked = transcript.speaker_voice_ids.get(&speaker).cloned();
                    let existing = voices.voices.iter().position(|v| {
                        (own_voice || !v.is_me)
                            && linked
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
                                sample_origins: Default::default(),
                                identity_revision: 0,
                            });
                            voices.voices.len() - 1
                        });
                        if own_voice {
                            for voice in &mut voices.voices {
                                voice.is_me = false;
                            }
                        }
                        let voice = &mut voices.voices[index];
                        if let Some(embedding) = embedding {
                            add_sample(voice, embedding, &sample_source);
                        }
                        if embedding.is_some()
                            && let Some(clusters) = &mut clusters
                        {
                            clusters
                                .origins
                                .insert(speaker.clone(), sample_source.clone());
                        }
                        voice.is_me |= own_voice;
                        voice.updated_at = Utc::now();
                        let voice_id = voice.id.clone().expect("prepared identity");
                        transcript
                            .speaker_voice_ids
                            .insert(speaker, voice_id.clone());
                        if everywhere {
                            if voice.name != name.trim() {
                                voice.identity_revision = voice.identity_revision.saturating_add(1);
                            }
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

#[cfg(test)]
mod biometric_rollback_tests {
    use super::*;
    use crate::{Channel, MeetingMeta, Segment, Source, SpeakerId, VoiceEnrollmentId};
    fn meeting(library: &Library, speaker: &str) -> (Meeting, Transcript) {
        let meeting = library
            .create_meeting(MeetingMeta::new("Нейтральная встреча", Utc::now()))
            .unwrap();
        let mut text = Transcript::default();
        text.segments.push(Segment {
            id: uuid::Uuid::now_v7(),
            speaker: SpeakerId(speaker.into()),
            channel: if speaker == "me" {
                Channel::Mic
            } else {
                Channel::System
            },
            start_ms: 0,
            end_ms: 7000,
            text: "Нейтральный пример речи".into(),
            source: Source::Final,
            edited: false,
            lang: None,
        });
        text.rename_speaker(speaker, "Олег Сидоров");
        meeting.write_transcript(&text).unwrap();
        (meeting, text)
    }
    fn enroll(
        library: &Library,
        meeting: &Meeting,
        text: &Transcript,
        vector: Vec<f32>,
    ) -> VoiceEnrollmentId {
        let id = VoiceEnrollmentId::new(
            meeting.read_meta().unwrap().id,
            meeting.load_transcript().unwrap().revision,
        );
        library
            .remember_tolk_voices(
                meeting,
                text,
                vec![VoiceSample {
                    speaker: "ktalk-person".into(),
                    name: "Участник".into(),
                    embedding: vector,
                    own: false,
                }],
                id,
                || false,
            )
            .unwrap();
        id
    }
    #[test]
    fn undo_removes_created_records_and_the_sync_cache() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (meeting, text) = meeting(&library, "ktalk-person");
        let before = library.read_voices().unwrap();
        let cache = library.read_meeting_voices(&meeting).unwrap();
        let id = enroll(&library, &meeting, &text, vec![1.0, 0.0]);
        assert_eq!(library.read_voices().unwrap().voices.len(), 1);
        assert!(library.undo_tolk_voices(&meeting, id).unwrap().is_empty());
        assert_eq!(library.read_voices().unwrap(), before);
        assert_eq!(library.read_meeting_voices(&meeting).unwrap(), cache);
        assert!(!library.tolk_enrollment_active(id).unwrap());
    }
    #[test]
    fn undo_keeps_other_meetings_and_manual_references_even_for_equal_samples() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (first, a) = meeting(&library, "ktalk-person");
        let (second, b) = meeting(&library, "ktalk-person");
        let id = enroll(&library, &first, &a, vec![1.0, 0.0]);
        let other = enroll(&library, &second, &b, vec![1.0, -0.0]);
        assert!(library.undo_tolk_voices(&first, id).unwrap().is_empty());
        let bank = library.read_voices().unwrap();
        assert_eq!(bank.voices.len(), 1);
        assert_eq!(bank.voices[0].embeddings, vec![vec![1.0, 0.0]]);
        assert!(
            bank.voices[0]
                .sample_origins
                .values()
                .any(|origins| origins.contains(&sync_source(other)))
        );
        let (manual, mut text) = meeting(&library, "s1");
        text.speaker_voice_ids
            .insert("s1".into(), "ktalk-person".into());
        manual.write_transcript(&text).unwrap();
        library
            .write_meeting_voices(&manual, BTreeMap::from([("s1".into(), vec![1.0, 0.0])]))
            .unwrap();
        library
            .edit_speakers(
                &manual,
                &text,
                crate::SpeakerEdit::Rename {
                    speaker: "s1".into(),
                    name: "Участник".into(),
                    is_me: false,
                    everywhere: false,
                },
                true,
            )
            .unwrap();
        assert!(library.undo_tolk_voices(&second, other).unwrap().is_empty());
        let bank = library.read_voices().unwrap();
        assert_eq!(bank.voices.len(), 1);
        assert_eq!(bank.voices[0].embeddings, vec![vec![1.0, 0.0]]);
    }
    #[test]
    fn changed_identity_is_left_for_review_but_transcript_can_be_restored() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (meeting, text) = meeting(&library, "ktalk-person");
        let id = enroll(&library, &meeting, &text, vec![1.0, 0.0]);
        let mut bank = library.read_voices().unwrap();
        bank.voices[0].name = "Изменённое имя".into();
        bank.voices[0].identity_revision += 1;
        write_json(&library.private_state_dir().join("voices.json"), &bank).unwrap();
        meeting.restore_transcript(&text, id.revision).unwrap();
        let review = library.undo_tolk_voices(&meeting, id).unwrap();
        assert_eq!(review.len(), 1);
        assert!(review[0].contains("Изменённое имя"));
        assert_eq!(library.read_voices().unwrap(), bank);
        assert_eq!(meeting.read_transcript().unwrap(), text);
    }
    #[test]
    fn undo_restores_the_sample_cap_and_resumed_units_preserve_other_sources() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (first, a) = meeting(&library, "ktalk-person");
        let (second, b) = meeting(&library, "ktalk-person");
        for index in 1..=8 {
            enroll(&library, &first, &a, vec![index as f32, 1.0]);
        }
        let before = library.read_voices().unwrap();
        let id = enroll(&library, &first, &a, vec![10.0, 1.0]);
        assert!(library.undo_tolk_voices(&first, id).unwrap().is_empty());
        assert_eq!(library.read_voices().unwrap(), before);
        let unit = enroll(&library, &first, &a, vec![11.0, 1.0]);
        enroll(&library, &second, &b, vec![12.0, 1.0]);
        library
            .remember_tolk_voices(
                &first,
                &a,
                vec![VoiceSample {
                    speaker: "ktalk-person".into(),
                    name: "Участник".into(),
                    embedding: vec![11.0, 1.0],
                    own: false,
                }],
                unit,
                || false,
            )
            .unwrap();
        assert!(library.undo_tolk_voices(&first, unit).unwrap().is_empty());
        let bank = library.read_voices().unwrap();
        assert!(bank.voices[0].embeddings.contains(&vec![12.0, 1.0]));
        assert!(!bank.voices[0].embeddings.contains(&vec![11.0, 1.0]));
    }
    #[test]
    fn manual_and_guest_naming_cannot_add_samples_to_own_voice_by_name() {
        let root = tempfile::tempdir().unwrap();
        let library = Library::open(root.path()).unwrap();
        let (own, text) = meeting(&library, "me");
        library
            .write_meeting_voices(&own, BTreeMap::from([("me".into(), vec![1.0, 0.0])]))
            .unwrap();
        library.remember_speakers_owned(&own, &text, true).unwrap();
        let original = library.read_voices().unwrap().voices[0].clone();
        for speaker in ["s1", "ktalk-guest"] {
            let (foreign, text) = meeting(&library, speaker);
            library
                .write_meeting_voices(&foreign, BTreeMap::from([(speaker.into(), vec![0.0, 1.0])]))
                .unwrap();
            library
                .edit_speakers(
                    &foreign,
                    &text,
                    crate::SpeakerEdit::Rename {
                        speaker: speaker.into(),
                        name: original.name.clone(),
                        is_me: false,
                        everywhere: false,
                    },
                    true,
                )
                .unwrap();
            let bank = library.read_voices().unwrap();
            assert_eq!(
                bank.voices.iter().find(|voice| voice.is_me).unwrap(),
                &original
            );
            assert_eq!(bank.voices.len(), 2);
            assert!(
                bank.voices
                    .iter()
                    .any(|voice| !voice.is_me && voice.embeddings.contains(&vec![0.0, 1.0]))
            );
        }
    }
}

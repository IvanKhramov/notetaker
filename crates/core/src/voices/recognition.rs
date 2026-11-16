use super::*;

impl VoiceLibrary {
    pub fn ranked(&self, embedding: &[f32]) -> Vec<(&Voice, f32)> {
        if self.model != VOICE_MODEL {
            return Vec::new();
        }
        let mut ranked: Vec<_> = self
            .voices
            .iter()
            .filter_map(|voice| {
                voice
                    .embeddings
                    .iter()
                    .filter_map(|sample| cosine(sample, embedding))
                    .max_by(f32::total_cmp)
                    .map(|score| (voice, score))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        ranked
    }
    pub fn recognition_counts(&self) -> (usize, usize) {
        (
            self.recognition_checks
                .values()
                .filter(|corrected| **corrected)
                .count(),
            self.recognition_checks.len(),
        )
    }
    /// Never enroll here. Samples remain in the per-meeting cache until confirmation.
    pub fn recognize(
        &self,
        transcript: &mut Transcript,
        embeddings: &BTreeMap<String, Vec<f32>>,
        automatic: bool,
    ) {
        let mut used: BTreeSet<_> = transcript
            .speaker_identities()
            .filter(|(id, _)| {
                transcript
                    .segments
                    .iter()
                    .any(|segment| segment.speaker.0 == *id)
            })
            .filter_map(|(_, identity)| identity.voice_id.map(str::to_owned))
            .collect();
        for voice in &self.voices {
            if let Some(id) = &voice.id
                && (voice
                    .tolk_identities
                    .iter()
                    .any(|alias| used.contains(alias))
                    || transcript.speaker_identities().any(|(speaker, identity)| {
                        same_name(identity.name, &voice.name)
                            && transcript
                                .segments
                                .iter()
                                .any(|segment| segment.speaker.0 == speaker)
                    }))
            {
                used.insert(id.clone());
            }
        }
        let mut matches: Vec<_> = embeddings
            .iter()
            .filter_map(|(speaker, embedding)| {
                if !transcript.segments.iter().any(|s| s.speaker.0 == *speaker)
                    || transcript.speaker_identity(speaker).is_some()
                    || transcript.ktalk_sources.contains_key(speaker)
                    || transcript
                        .segments
                        .iter()
                        .any(|s| s.speaker.0 == *speaker && s.channel == crate::Channel::Mic)
                {
                    return None;
                }
                let ranked = self.ranked(embedding);
                let &(voice, score) = ranked.first()?;
                let margin = ranked.get(1).map_or(1.0, |(_, next)| score - next);
                (score >= SUGGESTION_THRESHOLD && margin >= 0.08)
                    .then_some((speaker, voice, score, margin))
            })
            .collect();
        matches.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(b.0)));
        for (speaker, voice, score, margin) in matches {
            transcript
                .speaker_suggestions
                .insert(speaker.clone(), voice.name.clone());
            let Some(id) = &voice.id else {
                continue;
            };
            if automatic
                && score >= AUTOMATIC_THRESHOLD
                && margin >= AUTOMATIC_MARGIN
                && !transcript.voice_rejected.contains(speaker)
                && used.insert(id.clone())
            {
                transcript.speaker_voice_ids.remove(speaker);
                transcript.speaker_suggestions.remove(speaker);
                transcript.voice_recognitions.insert(
                    speaker.clone(),
                    crate::VoiceRecognition {
                        name: voice.name.clone(),
                        voice_id: id.clone(),
                        similarity_milli: (score * 1000.0).round().clamp(0.0, 1000.0) as u16,
                    },
                );
            } else if !transcript.voice_rejected.contains(speaker) {
                transcript
                    .speaker_voice_ids
                    .insert(speaker.clone(), id.clone());
            }
        }
    }
}
impl Transcript {
    pub fn reject_voice(&mut self, speaker: &str) -> String {
        if let Some(mark) = self.voice_recognitions.remove(speaker) {
            self.speaker_voice_ids.remove(speaker);
            self.voice_rejected.insert(speaker.into());
            let target = if (speaker == "me" || self.self_speaker.as_deref() == Some(speaker))
                && !self
                    .segments
                    .iter()
                    .any(|s| s.speaker.0 == speaker && s.channel == crate::Channel::Mic)
            {
                let target = self.new_speaker_id();
                for segment in self.segments.iter_mut().filter(|s| s.speaker.0 == speaker) {
                    segment.speaker.0.clone_from(&target);
                }
                self.self_speaker = None;
                self.speaker_suggestions.remove(speaker);
                self.voice_rejected.insert(target.clone());
                target
            } else {
                speaker.into()
            };
            self.speaker_suggestions.insert(target.clone(), mark.name);
            return target;
        }
        speaker.into()
    }
}
impl Library {
    /// Restore recognition from the local cache after a writer dropped optional fields.
    pub fn recognize_meeting(&self, meeting: &Meeting, automatic: bool) -> Result<()> {
        let cache = self.read_meeting_voices(meeting)?;
        if cache.model != VOICE_MODEL || cache.embeddings.is_empty() {
            return Ok(());
        }
        let bank = self.prepare_voices()?;
        let mut changed = false;
        meeting.update_transcript(|transcript| {
            let before = transcript.clone();
            bank.recognize(transcript, &cache.embeddings, automatic);
            changed |= *transcript != before;
        })?;
        if changed {
            self.reindex_incremental()?;
        }
        Ok(())
    }
    /// A stable local key prevents repeated sync from inflating the comparison counter.
    pub fn review_voice_names(
        &self,
        meeting: &Meeting,
        before: &Transcript,
        after: &Transcript,
    ) -> Result<()> {
        if before.voice_recognitions.is_empty() {
            return Ok(());
        }
        let path = self.private_state_dir().join("voices.json");
        let lock = meeting_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let _files = self.lock_voice_files()?;
        let mut bank = self.read_voices()?;
        let old = bank.clone();
        let meeting_id = meeting.read_meta()?.id;
        for (speaker, mark) in &before.voice_recognitions {
            let mut truth = BTreeSet::new();
            for segment in before.segments.iter().filter(|s| s.speaker.0 == *speaker) {
                if let Some(updated) = after.segments.iter().find(|s| s.id == segment.id)
                    && after.ktalk_sources.contains_key(&updated.speaker.0)
                {
                    truth.insert(updated.speaker.0.clone());
                }
            }
            if truth.is_empty() {
                continue;
            }
            let Some(voice) = bank
                .voices
                .iter()
                .find(|voice| voice.id.as_deref() == Some(&mark.voice_id))
            else {
                continue;
            };
            // A local UUID alone cannot prove a relationship to a Tolk identity.
            if !mark.voice_id.starts_with("ktalk-") && voice.tolk_identities.is_empty() {
                continue;
            }
            let corrected = truth
                .iter()
                .any(|id| voice.id.as_ref() != Some(id) && !voice.tolk_identities.contains(id));
            let source = truth
                .iter()
                .filter_map(|id| after.ktalk_sources.get(id))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(":");
            bank.recognition_checks
                .entry(format!("{meeting_id}:{speaker}:{}:{source}", mark.voice_id))
                .and_modify(|old| *old |= corrected)
                .or_insert(corrected);
        }
        if bank != old {
            write_json(&path, &bank)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bank() -> VoiceLibrary {
        let mut bank = VoiceLibrary::default();
        bank.enroll(
            VoiceKey::Stable("ktalk-a"),
            "Анна Петрова",
            false,
            &vec![1.0, 0.0],
        )
        .unwrap();
        bank
    }
    fn transcript() -> Transcript {
        let mut t = Transcript::default();
        for id in ["s1", "s2", "me"] {
            t.segments.push(crate::Segment {
                id: uuid::Uuid::now_v7(),
                speaker: crate::SpeakerId(id.into()),
                channel: if id == "me" {
                    crate::Channel::Mic
                } else {
                    crate::Channel::System
                },
                start_ms: 0,
                end_ms: 6000,
                text: "Нейтральная реплика".into(),
                source: crate::Source::Final,
                edited: false,
                lang: None,
            });
        }
        t
    }
    #[test]
    fn strong_names_weak_suggestions_protection_and_duplicate_person() {
        let bank = bank();
        let mut t = transcript();
        let samples = BTreeMap::from([
            ("s1".into(), vec![0.8, 0.6]),
            ("s2".into(), vec![0.99, 0.1]),
            ("me".into(), vec![1.0, 0.0]),
        ]);
        bank.recognize(&mut t, &samples, true);
        assert_eq!(t.speaker_name("s2"), "Анна Петрова");
        assert!(!t.speaker_names.contains_key("s1"));
        assert!(t.speaker_suggestions.contains_key("s1"));
        assert!(!t.speaker_names.contains_key("me"));
        assert_eq!(t.voice_recognitions.len(), 1);
        let _ = t.reject_voice("s2");
        assert!(!t.speaker_names.contains_key("s2"));
        assert!(!t.speaker_voice_ids.contains_key("s2"));
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s2".into(), vec![1.0, 0.0])]),
            true,
        );
        assert!(!t.speaker_names.contains_key("s2"));
        assert!(!t.speaker_voice_ids.contains_key("s2"));
        assert!(t.speaker_suggestions.contains_key("s2"));
        let mut t = transcript();
        let weak = vec![0.68, (1.0f32 - 0.68 * 0.68).sqrt()];
        bank.recognize(&mut t, &BTreeMap::from([("s1".into(), weak)]), true);
        assert!(!t.speaker_names.contains_key("s1"));
        assert!(t.speaker_suggestions.contains_key("s1"));
        t.rename_speaker("s1", "Oleg Sidorov");
        t.ktalk_sources.insert("s2".into(), "recording".into());
        t.speaker_names.insert("s2".into(), "Участник Толка".into());
        bank.recognize(&mut t, &samples, true);
        assert_eq!(t.speaker_name("s1"), "Oleg Sidorov");
        assert_eq!(t.speaker_name("s2"), "Участник Толка");
    }
    #[test]
    fn margin_and_toggle_do_not_auto_name_and_llm_gets_only_strong_names() {
        let mut bank = bank();
        bank.enroll(
            VoiceKey::Stable("ktalk-b"),
            "Oleg Sidorov",
            false,
            &vec![0.91, (1.0f32 - 0.91 * 0.91).sqrt()],
        )
        .unwrap();
        let mut t = transcript();
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s1".into(), vec![1.0, 0.0])]),
            true,
        );
        assert!(t.voice_recognitions.is_empty()); // Margin is below .10, even at cosine 1.
        let mut t = transcript();
        let bank = super::tests::bank();
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s1".into(), vec![1.0, 0.0])]),
            false,
        );
        assert!(t.speaker_names.is_empty());
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s1".into(), vec![1.0, 0.0])]),
            true,
        );
        t.speaker_suggestions
            .insert("s2".into(), "Oleg Sidorov".into());
        assert!(t.named_text().contains("Анна Петрова"));
        assert!(!t.named_text().contains("Oleg Sidorov"));
    }
    #[test]
    fn manual_correction_drops_the_guess_link_and_rejected_voice_can_be_selected_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let meeting = lib
            .create_meeting(crate::MeetingMeta::new("Нейтральная встреча", Utc::now()))
            .unwrap();
        let mut bank = bank();
        bank.enroll(
            VoiceKey::Stable("ktalk-b"),
            "Oleg Sidorov",
            false,
            &vec![0.0, 1.0],
        )
        .unwrap();
        write_json(&lib.private_state_dir().join("voices.json"), &bank).unwrap();
        let mut t = transcript();
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s1".into(), vec![1.0, 0.0])]),
            true,
        );
        meeting.write_transcript(&t).unwrap();
        lib.write_meeting_voices(&meeting, BTreeMap::from([("s1".into(), vec![0.8, 0.6])]))
            .unwrap();
        let edit = SpeakerEdit::Rename {
            speaker: "s1".into(),
            name: "Мария Иванова".into(),
            is_me: false,
            everywhere: false,
        };
        assert!(bank.speaker_name_status(&t, &edit, false).is_none());
        lib.edit_speakers(&meeting, &t, edit, true).unwrap();
        let corrected = meeting.read_transcript().unwrap();
        assert!(corrected.voice_recognitions.is_empty());
        assert_ne!(corrected.speaker_voice_ids["s1"], "ktalk-a");
        assert_eq!(lib.read_voices().unwrap().voices[0], bank.voices[0]);

        // Start again with the original recognition, then explicitly choose another identity.
        meeting.write_transcript(&t).unwrap();
        lib.edit_speakers(
            &meeting,
            &t,
            SpeakerEdit::RejectVoice {
                speaker: "s1".into(),
            },
            true,
        )
        .unwrap();
        let rejected = meeting.read_transcript().unwrap();
        lib.edit_speakers(
            &meeting,
            &rejected,
            SpeakerEdit::SelectVoice {
                speaker: "s1".into(),
                voice_id: "ktalk-b".into(),
            },
            true,
        )
        .unwrap();
        let selected = meeting.read_transcript().unwrap();
        assert_eq!(selected.speaker_name("s1"), "Oleg Sidorov");
        assert_eq!(selected.speaker_voice_ids["s1"], "ktalk-b");
        assert!(selected.voice_recognitions.is_empty());
    }
    #[test]
    fn no_sample_write_until_confirmation_then_tolk_reviews_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let meeting = lib
            .create_meeting(crate::MeetingMeta::new("Нейтральная встреча", Utc::now()))
            .unwrap();
        let bank = bank();
        write_json(&lib.private_state_dir().join("voices.json"), &bank).unwrap();
        let mut t = transcript();
        let embedding = vec![0.8, 0.6];
        bank.recognize(
            &mut t,
            &BTreeMap::from([("s1".into(), embedding.clone())]),
            true,
        );
        meeting.write_transcript(&t).unwrap();
        lib.write_meeting_voices(&meeting, BTreeMap::from([("s1".into(), embedding)]))
            .unwrap();
        let before = fs::read(lib.private_state_dir().join("voices.json")).unwrap();
        lib.remember_speakers(&meeting, &t).unwrap();
        assert_eq!(
            fs::read(lib.private_state_dir().join("voices.json")).unwrap(),
            before
        );
        lib.edit_speakers(
            &meeting,
            &t,
            SpeakerEdit::Rename {
                speaker: "s1".into(),
                name: "Анна Петрова".into(),
                is_me: false,
                everywhere: false,
            },
            true,
        )
        .unwrap();
        assert!(
            meeting
                .read_transcript()
                .unwrap()
                .voice_recognitions
                .is_empty()
        );
        assert_eq!(lib.read_voices().unwrap().voices[0].embeddings.len(), 2);
        let mut after = t.clone();
        for s in &mut after.segments {
            if s.speaker.0 == "s1" {
                s.speaker.0 = "ktalk-b".into();
            }
        }
        after
            .speaker_names
            .insert("ktalk-b".into(), "Oleg Sidorov".into());
        after
            .ktalk_sources
            .insert("ktalk-b".into(), "recording".into());
        lib.review_voice_names(&meeting, &t, &after).unwrap();
        lib.review_voice_names(&meeting, &t, &after).unwrap();
        assert_eq!(lib.read_voices().unwrap().recognition_counts(), (1, 1));
    }
}

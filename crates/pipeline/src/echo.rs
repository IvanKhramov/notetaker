use notetaker_core::{Channel, Segment};

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn duplicate(mic: &[String], system: &[String]) -> bool {
    if mic.len() < 3 || system.is_empty() {
        return false;
    }
    let protected = |words: &[String]| -> Vec<String> {
        words
            .iter()
            .filter(|word| {
                matches!(word.as_str(), "не" | "нет" | "ни" | "no" | "not" | "never")
                    || word.chars().any(char::is_numeric)
            })
            .cloned()
            .collect()
    };
    if protected(mic) != protected(system) {
        return false;
    }
    // Token edit distance preserves order, repetitions and meaningful omissions.
    let mut row: Vec<_> = (0..=system.len()).collect();
    for (i, a) in mic.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, b) in system.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (previous + usize::from(a != b))
                .min(row[j] + 1)
                .min(old + 1);
            previous = old;
        }
    }
    let similarity = 1.0 - row[system.len()] as f32 / mic.len().max(system.len()) as f32;
    if similarity < 0.85 {
        return false;
    }
    // Require the entire mic phrase in order. Extra mic words may be the user
    // talking over the remote voice; similarity alone would erase that evidence.
    let mut matched = 0;
    for token in system {
        if matched < mic.len() && *token == mic[matched] {
            matched += 1;
        }
    }
    matched == mic.len()
}

/// Drop only unedited mic evidence supported by overlapping system text.
/// The 300 ms tolerance covers speaker-to-microphone acoustic/ASR lag.
pub fn suppress_echo(segments: &mut Vec<Segment>) {
    let mut system: Vec<_> = segments
        .iter()
        .filter(|s| s.channel == Channel::System && !s.text.trim().is_empty())
        .map(|s| (s.start_ms, s.end_ms, tokens(&s.text)))
        .collect();
    system.sort_by_key(|s| s.0);
    segments.retain(|mic| {
        if mic.channel != Channel::Mic || mic.edited {
            return true;
        }
        let words = tokens(&mic.text);
        let overlap: Vec<_> = system
            .iter()
            .filter(|(start, end, _)| {
                *start < mic.end_ms.saturating_add(300) && mic.start_ms < end.saturating_add(300)
            })
            .collect();
        let joined: Vec<_> = overlap.iter().flat_map(|s| s.2.iter().cloned()).collect();
        !duplicate(&words, &joined) && !overlap.iter().any(|s| duplicate(&words, &s.2))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Source, SpeakerId};
    use uuid::Uuid;

    fn segment(channel: Channel, start: u64, end: u64, text: &str) -> Segment {
        Segment {
            id: Uuid::now_v7(),
            channel,
            speaker: SpeakerId("test".into()),
            start_ms: start,
            end_ms: end,
            text: text.into(),
            source: Source::Final,
            edited: false,
        }
    }

    #[test]
    fn negations_numbers_and_meaningful_omissions_survive() {
        for (mic, system) in [
            ("мы будем переносить релиз", "мы не будем переносить релиз"),
            ("мы не будем переносить релиз", "мы будем переносить релиз"),
            ("we will move the release", "we will not move the release"),
            ("we never move the release", "we move the release"),
            ("no we move the release", "we move the release"),
            ("нет мы переносим релиз", "мы переносим релиз"),
            ("ни мы ни они", "мы и они"),
            (
                "перенесём 12 встреч на завтра",
                "перенесём 13 встреч на завтра",
            ),
            (
                "мы согласны с этим планом",
                "мы согласны только с частью этого плана",
            ),
        ] {
            let mut segments = vec![
                segment(Channel::System, 0, 4000, system),
                segment(Channel::Mic, 0, 4000, mic),
            ];
            suppress_echo(&mut segments);
            assert_eq!(segments.len(), 2, "{mic} / {system}");
        }
        assert!(duplicate(
            &tokens("мы не будем переносить релиз 12"),
            &tokens("Мы не будем переносить релиз 12!")
        ));
    }

    #[test]
    fn russian_echo_lag_punctuation_and_split_phrases() {
        let mut segments = vec![
            segment(Channel::System, 0, 1000, "Обновим Deckhouse,"),
            segment(Channel::System, 1000, 2000, "после встречи."),
            segment(Channel::Mic, 200, 2200, "Обновим deckhouse после встречи!"),
            segment(Channel::Mic, 2200, 3000, "После встречи"), // Short replies survive.
            segment(Channel::Mic, 3000, 4000, "Обновим Deckhouse после встречи"),
        ];
        suppress_echo(&mut segments);
        assert_eq!(segments.len(), 4);
        assert!(!segments.iter().any(|s| s.start_ms == 200));
    }

    #[test]
    fn simultaneous_speech_partial_overlap_and_edits_survive() {
        let mut segments = vec![
            segment(
                Channel::System,
                0,
                4000,
                "Обновим кластер после встречи сегодня вечером",
            ),
            segment(
                Channel::Mic,
                100,
                3900,
                "Обновим кластер после встречи но завтра утром",
            ),
            segment(
                Channel::Mic,
                100,
                3900,
                "Нет сначала проверим резервную копию",
            ),
            segment(
                Channel::Mic,
                100,
                3900,
                "Обновим кластер после встречи сегодня вечером я против",
            ),
            segment(Channel::Mic, 3900, 4200, "После встречи сегодня вечером"),
        ];
        segments[4].edited = true;
        let original = segments.clone();
        suppress_echo(&mut segments);
        assert_eq!(segments, original);
        let mut partial = vec![
            original[0].clone(),
            segment(Channel::Mic, 3900, 4800, "После встречи сегодня вечером"),
        ];
        suppress_echo(&mut partial);
        // A short fragment below the similarity threshold is ambiguous evidence.
        assert_eq!(partial.len(), 2);
    }
}

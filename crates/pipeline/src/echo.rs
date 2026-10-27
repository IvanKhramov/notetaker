use std::collections::HashSet;

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
    let a: HashSet<_> = mic.iter().collect();
    let b: HashSet<_> = system.iter().collect();
    let jaccard = a.intersection(&b).count() as f32 / a.union(&b).count() as f32;
    if jaccard < 0.6 {
        return false;
    }
    // Require the entire mic phrase in order. Extra mic words may be the user
    // talking over the remote voice; Jaccard alone would erase that evidence.
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
pub(crate) fn suppress_echo(segments: &mut Vec<Segment>) {
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
        assert_eq!(partial.len(), 1);
    }
}

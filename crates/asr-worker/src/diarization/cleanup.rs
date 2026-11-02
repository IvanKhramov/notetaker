//! Conservative repair of complete-linkage fragments, without a speaker limit.
use std::collections::BTreeMap;

use notetaker_asr_proto::SpeakerTurn;

// Turn centroids are less variable than sherpa's short window embeddings.
// Weaker matches need a margin as well as similarity, not just a small cluster.
const STRONG_MATCH: f32 = 0.80;
const SAME_VOICE: f32 = 0.60;
const BRIEF_MATCH: f32 = 0.50;
const MATCH_MARGIN: f32 = 0.10;
const BRIEF_MS: u64 = 6000;

fn similarity(centroids: &BTreeMap<String, Vec<f32>>, a: &str, b: &str) -> Option<f32> {
    let (a, b) = (centroids.get(a)?, centroids.get(b)?);
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let score: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    score.is_finite().then_some(score)
}

fn overlap(turns: &[SpeakerTurn], aliases: &BTreeMap<String, String>, a: &str, b: &str) -> bool {
    turns.iter().filter(|t| aliases[&t.speaker] == a).any(|x| {
        turns
            .iter()
            .filter(|t| aliases[&t.speaker] == b)
            .any(|y| x.start_ms < y.end_ms && y.start_ms < x.end_ms)
    })
}

fn merge(aliases: &mut BTreeMap<String, String>, from: &str, into: &str) {
    for id in aliases.values_mut() {
        if id == from {
            *id = into.into();
        }
    }
}

pub(super) fn reassign(turns: &mut [SpeakerTurn], centroids: &BTreeMap<String, Vec<f32>>) {
    let mut durations = BTreeMap::<String, u64>::new();
    for turn in turns.iter() {
        *durations.entry(turn.speaker.clone()).or_default() += turn.end_ms - turn.start_ms;
    }
    let total: u64 = durations.values().sum();
    let mut order: Vec<_> = durations.keys().cloned().collect();
    order.sort_by(|a, b| durations[b].cmp(&durations[a]).then_with(|| a.cmp(b)));
    let mut aliases: BTreeMap<_, _> = order.iter().map(|id| (id.clone(), id.clone())).collect();
    // Consolidate strong matches first so fragments of the same voice do not
    // compete with one another in the weaker pass's ambiguity check.
    for threshold in [STRONG_MATCH, SAME_VOICE] {
        let mut anchors = Vec::<String>::new();
        for id in &order {
            if aliases[id] != *id {
                continue;
            }
            let mut ranked: Vec<_> = anchors
                .iter()
                .filter_map(|anchor| similarity(centroids, id, anchor).map(|score| (anchor, score)))
                .collect();
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let target = ranked.first().filter(|(anchor, score)| {
                *score >= threshold
                    && (threshold == STRONG_MATCH || ranked.get(1).is_none_or(|(_, next)| score - next >= MATCH_MARGIN))
                    && !overlap(turns, &aliases, id, anchor)
                    // Every member of both groups must match. No transitive
                    // chain of similar voices may collapse dissimilar people.
                    && aliases.iter().filter(|(_, target)| *target == id).all(|(member, _)| {
                        aliases.iter().filter(|(_, target)| *target == *anchor).all(|(other, _)| {
                            similarity(centroids, member, other).is_some_and(|s| s >= threshold)
                        })
                    })
            }).map(|(anchor, _)| *anchor);
            if let Some(target) = target {
                merge(&mut aliases, id, target);
            } else {
                anchors.push(id.clone());
            }
        }
    }
    let anchors: Vec<_> = order
        .iter()
        .filter(|id| aliases[*id] == **id)
        .cloned()
        .collect();
    let mut sizes = BTreeMap::<String, u64>::new();
    for (id, duration) in &durations {
        *sizes.entry(aliases[id].clone()).or_default() += duration;
    }
    // Both limits are required: a real participant with little airtime in a
    // short meeting must not be removed just because their turns are brief.
    let brief = |ms: u64| ms < BRIEF_MS && ms.saturating_mul(10) < total;
    let substantial: Vec<_> = anchors.iter().filter(|id| !brief(sizes[*id])).collect();
    for id in anchors.iter().filter(|id| brief(sizes[*id])) {
        let mut ranked: Vec<_> = substantial
            .iter()
            .filter_map(|anchor| similarity(centroids, id, anchor).map(|score| (*anchor, score)))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        if let Some((target, score)) = ranked.first()
            && *score >= BRIEF_MATCH
            && ranked
                .get(1)
                .is_none_or(|(_, next)| score - next >= MATCH_MARGIN)
            && !overlap(turns, &aliases, id, target)
        {
            merge(&mut aliases, id, target);
        }
    }
    for turn in turns {
        turn.speaker.clone_from(&aliases[&turn.speaker]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: &str, start: u64, duration: u64) -> SpeakerTurn {
        SpeakerTurn {
            speaker: id.into(),
            start_ms: start,
            end_ms: start + duration,
        }
    }

    fn vector(x: f32, y: f32) -> Vec<f32> {
        let norm = x.hypot(y);
        vec![x / norm, y / norm]
    }

    #[test]
    fn brief_clear_match_moves_but_distinct_and_ambiguous_voices_stay() {
        let centroids = BTreeMap::from([
            ("a".into(), vector(1.0, 0.0)),
            ("b".into(), vector(0.0, 1.0)),
            ("fragment".into(), vector(0.55, -0.835)),
            ("quiet".into(), vector(-1.0, 0.0)),
            ("ambiguous".into(), vector(1.0, 1.0)),
        ]);
        let mut turns = vec![
            turn("a", 0, 60000),
            turn("b", 60000, 60000),
            turn("fragment", 120000, 2000),
            turn("quiet", 122000, 1000),
            turn("ambiguous", 123000, 1000),
        ];
        reassign(&mut turns, &centroids);
        assert_eq!(
            turns.iter().map(|t| t.speaker.as_str()).collect::<Vec<_>>(),
            ["a", "b", "a", "quiet", "ambiguous"]
        );
        assert_eq!(turns[2].start_ms, 120000);
        assert_eq!(turns[2].end_ms, 122000);
    }

    #[test]
    fn no_blind_merge_of_short_missing_or_overlapping_speech() {
        let centroids = BTreeMap::from([
            ("a".into(), vector(1.0, 0.0)),
            ("b".into(), vector(1.0, 0.0)),
        ]);
        let mut turns = vec![
            turn("a", 0, 60000),
            turn("b", 1000, 1000),
            turn("missing", 60000, 500),
        ];
        let original = turns.clone();
        reassign(&mut turns, &centroids);
        assert_eq!(turns, original);
        let mut short = vec![turn("a", 0, 5000), turn("b", 5000, 1000)];
        let centroids = BTreeMap::from([
            ("a".into(), vector(1.0, 0.0)),
            ("b".into(), vector(0.55, 0.835)),
        ]);
        let original = short.clone();
        reassign(&mut short, &centroids);
        assert_eq!(short, original);
    }

    #[test]
    fn similar_substantial_clusters_merge_without_chaining() {
        let centroids = BTreeMap::from([
            ("a".into(), vector(1.0, 0.0)),
            ("b".into(), vector(0.8, 0.6)),
            ("c".into(), vector(0.8, -0.6)),
        ]);
        let mut turns = vec![
            turn("a", 0, 30000),
            turn("b", 30000, 20000),
            turn("c", 50000, 10000),
        ];
        reassign(&mut turns, &centroids);
        assert_eq!(
            turns.iter().map(|t| t.speaker.as_str()).collect::<Vec<_>>(),
            ["a", "a", "c"]
        );
    }
}

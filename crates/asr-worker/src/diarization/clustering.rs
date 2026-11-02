//! Average-linkage on clean turn embeddings, followed by small-cluster assignment.
use std::collections::BTreeMap;

use notetaker_asr_proto::SpeakerTurn;

use super::normalize;

pub(crate) const MIN_CLEAN_MS: u64 = 1500;
const MAX_TRAINING: usize = 512;
const DISTANCE: f32 = 0.5;
const MIN_CLUSTER_MS: u64 = 6000;
const MIN_SPEECH_SHARE: f64 = 0.02;
const MAX_MINIMUM_SHARE: f64 = 0.10;

pub(crate) struct Evidence {
    pub embedding: Option<Vec<f32>>,
    pub clean_ms: u64,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

fn nearest(embedding: &[f32], centroids: &[Vec<f32>]) -> usize {
    (0..centroids.len())
        .max_by(|&a, &b| {
            cosine(embedding, &centroids[a]).total_cmp(&cosine(embedding, &centroids[b]))
        })
        .unwrap_or(0)
}

/// Auto uses a distance cut and duration floor; fixed count cuts the same tree
/// at N groups, retaining short voices rather than applying the auto floor.
fn centroids(evidence: &[&Evidence], total_ms: u64, speakers: Option<u32>) -> Vec<Vec<f32>> {
    let n = evidence.len();
    let mut groups: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let vectors: Vec<_> = evidence
        .iter()
        .map(|e| e.embedding.as_ref().unwrap())
        .collect();
    let mut distances = vec![f32::INFINITY; n * n];
    for i in 0..n {
        for j in 0..i {
            let distance = 1.0 - cosine(vectors[i], vectors[j]);
            distances[i * n + j] = distance;
            distances[j * n + i] = distance;
        }
    }
    let mut remaining = n;
    loop {
        if remaining <= speakers.map_or(1, |n| n as usize) {
            break;
        }
        let mut best = (0, 0, f32::INFINITY);
        for i in 0..n {
            if groups[i].is_empty() {
                continue;
            }
            for j in 0..i {
                if !groups[j].is_empty() && distances[i * n + j] < best.2 {
                    best = (i, j, distances[i * n + j]);
                }
            }
        }
        let (i, j, distance) = best;
        if !distance.is_finite() || (speakers.is_none() && distance > DISTANCE) {
            break;
        }
        let (a, b) = (groups[i].len() as f32, groups[j].len() as f32);
        for k in 0..n {
            if k == i || k == j || groups[k].is_empty() {
                continue;
            }
            let merged = (a * distances[i * n + k] + b * distances[j * n + k]) / (a + b);
            distances[i * n + k] = merged;
            distances[k * n + i] = merged;
        }
        let removed = std::mem::take(&mut groups[j]);
        groups[i].extend(removed);
        remaining -= 1;
    }
    let duration = |group: &Vec<usize>| group.iter().map(|&i| evidence[i].clean_ms).sum::<u64>();
    // As in pyannote's small-sample rule, the minimum must not exceed 10%
    // of training speech. A six-second floor erases people in brief clips.
    let floor = MIN_CLUSTER_MS
        .max((total_ms as f64 * MIN_SPEECH_SHARE) as u64)
        .min((total_ms as f64 * MAX_MINIMUM_SHARE) as u64);
    let largest = groups
        .iter()
        .enumerate()
        .max_by_key(|(_, g)| duration(g))
        .map(|(i, _)| i);
    groups
        .iter()
        .enumerate()
        .filter(|(i, g)| {
            !g.is_empty() && (speakers.is_some() || duration(g) >= floor || Some(*i) == largest)
        })
        .filter_map(|(_, group)| {
            let mut sum = vec![0.0; vectors[0].len()];
            for &i in group {
                for (s, v) in sum.iter_mut().zip(vectors[i]) {
                    *s += v;
                }
            }
            normalize(sum)
        })
        .collect()
}

pub(crate) fn reassign(
    turns: &mut [SpeakerTurn],
    evidence: &[Evidence],
    fallback: &BTreeMap<String, Vec<f32>>,
    speakers: Option<u32>,
) {
    let training: Vec<_> = evidence
        .iter()
        .filter(|e| e.clean_ms >= MIN_CLEAN_MS && e.embedding.is_some())
        .collect();
    if training.is_empty() {
        return;
    }
    // Fixed pseudo-random priorities avoid sampling only one voice in periodic
    // turn-taking (e.g. every second turn), while bounding cubic linkage work.
    let mut indices: Vec<_> = (0..training.len()).collect();
    if indices.len() > MAX_TRAINING {
        indices.sort_unstable_by_key(|&i| {
            let mut hash = (i as u64).wrapping_add(0x9e3779b97f4a7c15);
            hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d049bb133111eb);
            hash ^ (hash >> 31)
        });
        indices.truncate(MAX_TRAINING);
        indices.sort_unstable();
    }
    let selected: Vec<_> = indices.into_iter().map(|i| training[i]).collect();
    let total_ms = selected.iter().map(|e| e.clean_ms).sum();
    let centers = centroids(&selected, total_ms, speakers);
    if centers.is_empty() {
        return;
    }
    for (turn, e) in turns.iter_mut().zip(evidence) {
        // Short and overlapped turns borrow evidence from their original group.
        // Their own unreliable embeddings never create new training clusters.
        let vector = if e.clean_ms >= 500 {
            e.embedding.as_ref().or_else(|| fallback.get(&turn.speaker))
        } else {
            fallback.get(&turn.speaker).or(e.embedding.as_ref())
        };
        if let Some(vector) = vector {
            turn.speaker = format!("cluster{}", nearest(vector, &centers));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(x: f32, y: f32, clean_ms: u64) -> Evidence {
        Evidence {
            embedding: normalize(vec![x, y]),
            clean_ms,
        }
    }

    #[test]
    fn fixed_count_cuts_tree_without_distance_or_duration_floor() {
        let data = [
            evidence(1.0, 0.0, 8000),
            evidence(0.95, 0.05, 8000),
            evidence(0.0, 1.0, 8000),
            evidence(-1.0, 0.0, 1500),
        ];
        let refs: Vec<_> = data.iter().collect();
        for n in 1..=4 {
            assert_eq!(centroids(&refs, 25500, Some(n)).len(), n as usize);
        }
        // More voices than clean training turns cannot invent acoustic evidence.
        assert_eq!(centroids(&refs, 25500, Some(8)).len(), 4);
    }

    #[test]
    fn duration_floor_removes_outlier_but_keeps_two_sustained_voices() {
        let data = [
            evidence(1.0, 0.0, 4000),
            evidence(0.9, 0.1, 4000),
            evidence(0.0, 1.0, 8000),
            evidence(-1.0, 0.0, 1500),
        ];
        let refs: Vec<_> = data.iter().collect();
        let centers = centroids(&refs, 17500, None);
        assert_eq!(centers.len(), 2);
        assert_ne!(
            nearest(&[1.0, 0.0], &centers),
            nearest(&[0.0, 1.0], &centers)
        );
    }

    #[test]
    fn one_short_voice_keeps_a_centroid_and_no_clean_evidence_keeps_labels() {
        let data = [evidence(1.0, 0.0, 1600), evidence(0.9, 0.1, 1600)];
        assert_eq!(
            centroids(&data.iter().collect::<Vec<_>>(), 3200, None).len(),
            1
        );
        let mut turns = vec![SpeakerTurn {
            start_ms: 0,
            end_ms: 500,
            speaker: "s1".into(),
        }];
        reassign(
            &mut turns,
            &[evidence(1.0, 0.0, 500)],
            &BTreeMap::new(),
            None,
        );
        assert_eq!(turns[0].speaker, "s1");
    }

    #[test]
    fn brief_clip_does_not_erase_a_real_shorter_voice() {
        let data = [evidence(1.0, 0.0, 8000), evidence(0.0, 1.0, 3500)];
        assert_eq!(
            centroids(&data.iter().collect::<Vec<_>>(), 11500, None).len(),
            2
        );
    }

    #[test]
    fn bounded_training_does_not_alias_alternating_voices() {
        let data: Vec<_> = (0..1024)
            .map(|i| {
                if i % 2 == 0 {
                    evidence(1.0, 0.0, 2000)
                } else {
                    evidence(0.0, 1.0, 2000)
                }
            })
            .collect();
        let mut turns: Vec<_> = (0..1024)
            .map(|i| SpeakerTurn {
                start_ms: i * 2000,
                end_ms: (i + 1) * 2000,
                speaker: format!("old{i}"),
            })
            .collect();
        reassign(&mut turns, &data, &BTreeMap::new(), None);
        assert_ne!(turns[0].speaker, turns[1].speaker);
        assert!(
            turns
                .iter()
                .enumerate()
                .all(|(i, t)| t.speaker == turns[i % 2].speaker)
        );
    }

    #[test]
    fn short_turn_uses_group_evidence_and_timings_survive() {
        let mut turns = vec![
            SpeakerTurn {
                start_ms: 0,
                end_ms: 500,
                speaker: "old".into(),
            },
            SpeakerTurn {
                start_ms: 1000,
                end_ms: 9000,
                speaker: "long".into(),
            },
            SpeakerTurn {
                start_ms: 9000,
                end_ms: 17000,
                speaker: "other".into(),
            },
        ];
        let data = [
            evidence(0.0, 1.0, 499),
            evidence(1.0, 0.0, 8000),
            evidence(0.0, 1.0, 8000),
        ];
        reassign(
            &mut turns,
            &data,
            &BTreeMap::from([("old".into(), vec![1.0, 0.0])]),
            None,
        );
        assert_eq!(turns[0].speaker, turns[1].speaker);
        assert_ne!(turns[0].speaker, turns[2].speaker);
        assert_eq!((turns[0].start_ms, turns[0].end_ms), (0, 500));
    }
}

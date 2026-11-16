//! A resumable queue contains ids only; remote text and media are never persisted.
use super::{Client, Recording};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub struct LearningPlan {
    pub origin: String,
    pub recordings: Vec<Recording>,
    pub estimated_bytes: u64,
    pub cutoff: DateTime<Utc>,
}
pub fn learning_plan(client: &Client, cutoff: DateTime<Utc>) -> Result<LearningPlan> {
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    for page in 0..30 {
        let items = client.recordings("", page * 100, 100)?;
        ensure!(items.len() <= 100, "Некорректная страница записей Толка");
        let stop = items.len() < 100 || items.iter().all(|r| r.created_date < cutoff);
        let mut added = 0;
        for r in items {
            if seen.insert(r.id.clone()) {
                added += 1;
                ensure!(
                    !r.id.is_empty() && r.id.len() <= 512,
                    "Некорректный ID записи Толка"
                );
                if r.created_date >= cutoff
                    && r.status == "complete"
                    && r.transcription
                        .as_ref()
                        .is_some_and(|t| t.status == "complete")
                {
                    records.push(r);
                }
            }
        }
        if stop || added == 0 {
            let estimated_bytes = records
                .iter()
                .map(|r| (u64::from(r.duration) * 64_000).min(super::AUDIO_LIMIT))
                .sum();
            return Ok(LearningPlan {
                origin: client.origin(),
                recordings: records,
                estimated_bytes,
                cutoff,
            });
        }
    }
    anyhow::bail!("Достигнут лимит 30 страниц; список записей неполный")
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LearningQueue {
    #[serde(default)]
    pub days: u32,
    pub origin: String,
    pub cutoff: Option<DateTime<Utc>>,
    pub remaining: Vec<String>,
    pub processed: BTreeSet<String>,
    pub people: BTreeSet<String>,
    pub total: usize,
    pub attempted: usize,
    pub no_speech: usize,
    pub network_errors: usize,
    pub other_errors: usize,
    pub active: bool,
}
impl LearningQueue {
    pub fn begin(origin: String, plan: &LearningPlan) -> Self {
        Self {
            origin,
            cutoff: Some(plan.cutoff),
            remaining: plan.recordings.iter().map(|r| r.id.clone()).collect(),
            total: plan.recordings.len(),
            active: true,
            ..Default::default()
        }
    }
    pub fn finish_recording(&mut self, id: &str, people: Vec<String>, no_speech: bool) {
        self.remaining.retain(|next| next != id);
        self.processed.insert(id.into());
        self.people.extend(people);
        self.attempted += 1;
        self.no_speech += usize::from(no_speech);
        if self.remaining.is_empty() {
            self.active = false;
        }
    }
}

pub fn learning_source(origin: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(serde_json::to_vec(&(origin, id)).expect("strings serialize"))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn learning_origin(settings: &super::Settings) -> Result<String> {
    Ok(super::client::base_url(&settings.base_url)?
        .origin()
        .ascii_serialization())
}

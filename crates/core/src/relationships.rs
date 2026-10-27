//! Derived relationship views always check source metadata before loading content.
use crate::{Library, MeetingMeta, Mode, Person, Result, Task, Transcript, people::normalized};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MeetingSummary {
    pub meta: MeetingMeta,
    pub path: PathBuf,
    pub summary: String,
    pub open_tasks: usize,
    pub series_id: Option<String>,
    pub participants: BTreeSet<String>,
    #[serde(skip)]
    pub transcript: Transcript,
}
impl MeetingSummary {
    pub fn excerpt(&self) -> String {
        self.summary
            .lines()
            .filter(|s| !s.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(500)
            .collect()
    }
    pub fn has_person(&self, person: &Person) -> bool {
        self.meta.calendar_event.as_ref().is_some_and(|event| {
            event.attendees.iter().any(|a| {
                if a.email.trim().is_empty() {
                    person.matches(&a.name)
                } else {
                    person.emails.contains(&a.email.trim().to_lowercase())
                }
            })
        }) || self
            .transcript
            .speaker_names
            .values()
            .any(|name| person.matches(name))
            || self
                .transcript
                .speaker_voice_ids
                .values()
                .any(|id| person.voice_ids.contains(id))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersonPage {
    pub person: Person,
    pub meetings: Vec<MeetingSummary>,
    pub owned_tasks: Vec<Task>,
    pub requested_tasks: Vec<Task>,
    pub talk_ms: u64,
    pub segment_count: usize,
    pub turns: usize,
    pub longest_monologue_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub title: String,
    /// Newest first, including the reference meeting.
    pub meetings: Vec<MeetingSummary>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CarryOver {
    pub previous: MeetingSummary,
    pub decisions: String,
    pub tasks: Vec<Task>,
}
impl CarryOver {
    pub fn markdown(&self) -> String {
        let mut text = format!(
            "## С прошлой встречи\n\n{} ({})\n\n{}\n\n",
            self.previous.meta.title,
            self.previous.meta.started_at.format("%d.%m.%Y"),
            self.decisions
        );
        for task in &self.tasks {
            text.push_str(&format!(
                "- [ ] {} - {} - {}\n",
                task.text, task.owner, task.due_text
            ));
        }
        text
    }
}
/// Only explicitly labelled decisions are presented as decisions.
pub fn decisions(summary: &str) -> String {
    let mut active: Option<usize> = None;
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in summary.lines() {
        let trim = line.trim();
        if trim.starts_with("```") || trim.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let depth = trim.chars().take_while(|c| *c == '#').count();
        if depth > 0 {
            if active.is_some_and(|level| depth <= level) {
                active = None;
            }
            let lower = trim.to_lowercase();
            if lower.contains("решени")
                || lower.contains("договорились")
                || lower.contains("decisions")
            {
                active = Some(depth);
            }
        }
        if active.is_some()
            || trim.to_lowercase().starts_with("решили:")
            || trim.to_lowercase().starts_with("договорились:")
        {
            lines.push(line);
        }
    }
    lines.join("\n")
}
impl Library {
    pub fn meeting_summaries(&self, exclude_local: bool) -> Result<Vec<MeetingSummary>> {
        self.reindex_incremental()?;
        let tasks = self.list_tasks(Some(false), None, exclude_local)?;
        let mut summaries = Vec::new();
        for meeting in self.list_meetings()? {
            if meeting.validate_source_paths().is_err() {
                continue;
            }
            let meta = meeting.read_meta()?;
            if exclude_local && meta.mode == Mode::Local {
                continue;
            }
            let mut transcript = meeting.read_transcript()?;
            if !exclude_local
                && transcript
                    .speaker_ids()
                    .contains(transcript.self_speaker.as_deref().unwrap_or("me"))
            {
                transcript.default_self_name(&crate::configured_user_name()?);
            }
            let participants: BTreeSet<_> = if let Some(event) = &meta.calendar_event {
                event
                    .attendees
                    .iter()
                    .map(|a| {
                        normalized(if a.email.trim().is_empty() {
                            &a.name
                        } else {
                            &a.email
                        })
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                transcript
                    .speaker_names
                    .values()
                    .map(|s| normalized(s))
                    .filter(|s| !s.is_empty())
                    .collect()
            };
            let series_id = if let Some(event) = &meta.calendar_event {
                (!event.uid.trim().is_empty()).then(|| format!("uid:{}", event.uid))
            } else if !participants.is_empty() && !meta.title.trim().is_empty() {
                Some(format!(
                    "heuristic:{}",
                    serde_json::to_string(&(normalized(&meta.title), &participants))?
                ))
            } else {
                None
            };
            summaries.push(MeetingSummary {
                open_tasks: tasks.iter().filter(|t| t.meeting_id == meta.id).count(),
                meta,
                path: meeting.path().to_owned(),
                summary: meeting.read_enhanced()?,
                series_id,
                participants,
                transcript,
            });
        }
        summaries.sort_by_key(|m| (std::cmp::Reverse(m.meta.started_at), m.meta.id));
        Ok(summaries)
    }
    pub fn person_page(&self, person: Person, exclude_local: bool) -> Result<PersonPage> {
        let meetings: Vec<_> = self
            .meeting_summaries(exclude_local)?
            .into_iter()
            .filter(|m| m.has_person(&person))
            .collect();
        let tasks = self.list_tasks(None, None, exclude_local)?;
        let mut page = PersonPage {
            owned_tasks: tasks
                .iter()
                .filter(|t| !t.completed && person.matches(&t.owner))
                .cloned()
                .collect(),
            requested_tasks: tasks
                .into_iter()
                .filter(|t| person.matches(&t.requester))
                .collect(),
            person,
            meetings,
            talk_ms: 0,
            segment_count: 0,
            turns: 0,
            longest_monologue_ms: 0,
        };
        for meeting in &page.meetings {
            let mut transcript = meeting.transcript.clone();
            let speakers: BTreeSet<_> = transcript
                .speaker_ids()
                .into_iter()
                .filter(|s| {
                    transcript
                        .speaker_names
                        .get(s)
                        .is_some_and(|n| page.person.matches(n))
                        || transcript
                            .speaker_voice_ids
                            .get(s)
                            .is_some_and(|v| page.person.voice_ids.contains(v))
                })
                .collect();
            for segment in &mut transcript.segments {
                if speakers.contains(&segment.speaker.0) {
                    segment.speaker.0 = "person".into();
                }
            }
            if let Some(stats) = transcript
                .speaker_timeline()
                .speakers
                .iter()
                .find(|s| s.id == "person")
            {
                page.talk_ms += stats.total_ms;
                page.segment_count += stats.segment_count;
                page.turns += stats.turns;
                page.longest_monologue_ms =
                    page.longest_monologue_ms.max(stats.longest_monologue_ms);
            }
        }
        Ok(page)
    }
    pub fn series(&self, exclude_local: bool) -> Result<Vec<Series>> {
        let mut groups: BTreeMap<String, Series> = BTreeMap::new();
        for meeting in self.meeting_summaries(exclude_local)? {
            if let Some(id) = &meeting.series_id {
                let entry = groups.entry(id.clone()).or_insert_with(|| Series {
                    id: id.clone(),
                    title: meeting
                        .meta
                        .calendar_event
                        .as_ref()
                        .map_or_else(|| meeting.meta.title.clone(), |e| e.title.clone()),
                    meetings: Vec::new(),
                });
                entry.meetings.push(meeting);
            }
        }
        Ok(groups.into_values().collect())
    }
    /// Re-read carry-over at the explicit insertion boundary. Publish Local before
    /// publishing its text, and preserve externally changed notes via revisions.
    pub fn insert_carry_over(
        &self,
        meeting: &crate::Meeting,
        previous: uuid::Uuid,
        revision: crate::Revision,
    ) -> Result<()> {
        if meeting.path().parent() != Some(self.root()) {
            return Err(crate::Error::TaskEdit("Встреча вне библиотеки".into()));
        }
        let notes = meeting.load_notes()?;
        if notes.revision != revision {
            return Err(crate::Error::TaskEdit(
                "Заметки изменились; откройте встречу заново".into(),
            ));
        }
        let meta = meeting.read_meta()?;
        let series = self
            .series(false)?
            .into_iter()
            .find(|s| s.meetings.iter().any(|m| m.meta.id == meta.id))
            .ok_or_else(|| crate::Error::TaskEdit("Серия изменилась".into()))?;
        let carry = self
            .carry_over(&series, meta.started_at, false)?
            .ok_or_else(|| crate::Error::TaskEdit("Предыдущая встреча не найдена".into()))?;
        if carry.previous.meta.id != previous {
            return Err(crate::Error::TaskEdit(
                "Серия изменилась; обновите список".into(),
            ));
        }
        if carry.previous.meta.mode == Mode::Local {
            let mut meta = meeting.read_meta()?;
            meta.mode = Mode::Local;
            meeting.write_meta(&meta)?;
        }
        meeting.save_notes(
            &format!("{}\n\n{}", carry.markdown(), notes.value),
            revision,
        )?;
        Ok(())
    }
    pub fn carry_over(
        &self,
        series: &Series,
        started_at: DateTime<Utc>,
        exclude_local: bool,
    ) -> Result<Option<CarryOver>> {
        let Some(previous) = series.meetings.iter().find(|m| {
            m.meta.started_at < started_at && (!exclude_local || m.meta.mode != Mode::Local)
        }) else {
            return Ok(None);
        };
        let tasks = self
            .list_tasks(Some(false), None, exclude_local)?
            .into_iter()
            .filter(|t| t.meeting_id == previous.meta.id)
            .collect();
        Ok(Some(CarryOver {
            decisions: decisions(&previous.summary),
            previous: previous.clone(),
            tasks,
        }))
    }
}

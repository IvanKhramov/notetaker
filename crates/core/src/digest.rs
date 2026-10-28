use crate::{Library, MeetingSummary, Mode, Person, Result, Task, decisions};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

/// Inclusive local calendar start, exclusive end. Membership uses the caller's TZ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestPeriod {
    pub start: NaiveDate,
    pub end: NaiveDate,
}
impl DigestPeriod {
    pub fn week<T: TimeZone>(now: DateTime<T>, previous: bool) -> Self {
        let date = now.date_naive();
        let start = date
            - Duration::days(
                i64::from(date.weekday().num_days_from_monday()) + if previous { 7 } else { 0 },
            );
        Self {
            start,
            end: start + Duration::days(7),
        }
    }
    pub fn filename(&self) -> String {
        let week = self.start.iso_week();
        let monday = self.start.weekday() == chrono::Weekday::Mon;
        if monday && self.end == self.start + Duration::days(7) {
            format!("{}-W{:02}.md", week.year(), week.week())
        } else {
            format!("{}_{}.md", self.start, self.end)
        }
    }
    pub fn contains<T: TimeZone>(&self, time: DateTime<Utc>, zone: &T) -> bool {
        let date = time.with_timezone(zone).date_naive();
        date >= self.start && date < self.end
    }
}
#[derive(Clone, Debug, Default)]
pub struct DigestFilter {
    pub person: Option<Person>,
    pub series_id: Option<String>,
    pub folder: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Digest {
    pub period: DigestPeriod,
    pub meetings: Vec<MeetingSummary>,
    pub new_tasks: Vec<Task>,
    pub overdue_tasks: Vec<Task>,
    #[serde(default)]
    pub highlights: BTreeMap<String, BTreeSet<String>>,
    pub markdown: String,
}
impl Library {
    pub fn digest<T: TimeZone>(
        &self,
        period: DigestPeriod,
        filter: &DigestFilter,
        zone: &T,
        exclude_local: bool,
    ) -> Result<Digest> {
        if period.start >= period.end {
            return Err(crate::Error::InvalidIndex(
                "Начало периода должно быть раньше конца".into(),
            ));
        }
        let all = self.meeting_summaries(exclude_local)?;
        let relevant = |m: &&MeetingSummary| {
            crate::folders::matches_folder(&m.meta, filter.folder.as_deref())
                && filter.person.as_ref().is_none_or(|p| m.has_person(p))
                && filter
                    .series_id
                    .as_ref()
                    .is_none_or(|id| m.series_id.as_ref() == Some(id))
        };
        let eligible: BTreeSet<_> = all.iter().filter(relevant).map(|m| m.meta.id).collect();
        let meetings: Vec<_> = all
            .iter()
            .filter(relevant)
            .filter(|m| period.contains(m.meta.started_at, zone))
            .cloned()
            .collect();
        let ids: BTreeSet<_> = meetings.iter().map(|m| m.meta.id).collect();
        let tasks = self.list_tasks(None, None, exclude_local)?;
        let new_tasks = tasks
            .iter()
            .filter(|t| ids.contains(&t.meeting_id))
            .cloned()
            .collect();
        let cutoff = period
            .end
            .min(chrono::Utc::now().with_timezone(zone).date_naive());
        let overdue_tasks = tasks
            .into_iter()
            .filter(|t| {
                !t.completed
                    && eligible.contains(&t.meeting_id)
                    && t.due.is_some_and(|due| due < cutoff)
                    && all.iter().any(|m| {
                        m.meta.id == t.meeting_id
                            && m.meta.started_at.with_timezone(zone).date_naive() < period.end
                    })
            })
            .collect();
        let mut digest = Digest {
            period,
            meetings,
            new_tasks,
            overdue_tasks,
            markdown: String::new(),
            highlights: BTreeMap::new(),
        };
        let (name, aliases) = if exclude_local {
            (String::new(), String::new())
        } else {
            crate::identity::configured_identity()?
        };
        for person in self.people(exclude_local, &name, &aliases)? {
            let mut items: BTreeSet<_> = digest
                .meetings
                .iter()
                .filter(|m| m.has_person(&person))
                .map(|m| format!("{}: {}", m.meta.title, m.excerpt()))
                .collect();
            for task in digest
                .new_tasks
                .iter()
                .chain(&digest.overdue_tasks)
                .filter(|t| person.matches(&t.owner))
            {
                items.insert(format!("Задача: {} ({})", task.text, task.due_text));
            }
            if !items.is_empty() {
                let label = if person.emails.is_empty() {
                    person.name
                } else {
                    format!(
                        "{} <{}>",
                        person.name,
                        person.emails.into_iter().collect::<Vec<_>>().join(", ")
                    )
                };
                digest.highlights.insert(label, items);
            }
        }
        digest.markdown = digest.plain_markdown();
        Ok(digest)
    }
    pub fn save_digest(&self, digest: &Digest) -> Result<PathBuf> {
        let directory = self.root().join(".digests");
        fs::create_dir_all(&directory)?;
        let path = directory.join(digest.period.filename());
        crate::storage::atomic_write(&path, digest.markdown.as_bytes())?;
        Ok(path)
    }
}
impl Digest {
    pub fn plain_markdown(&self) -> String {
        let mut text = format!(
            "# Дайджест: {} - {} (не включая)\n\n## Встречи\n",
            self.period.start, self.period.end
        );
        for meeting in &self.meetings {
            text.push_str(&format!(
                "- {} {}{}\n",
                meeting
                    .meta
                    .started_at
                    .with_timezone(&chrono::Local)
                    .format("%d.%m %H:%M"),
                meeting.meta.title,
                if meeting.meta.mode == Mode::Local {
                    " (Local)"
                } else {
                    ""
                }
            ));
        }
        text.push_str("\n## Ключевые решения\n");
        for meeting in &self.meetings {
            let decisions = decisions(&meeting.summary);
            if !decisions.is_empty() {
                text.push_str(&format!("\n### {}\n{}\n", meeting.meta.title, decisions));
            }
        }
        for (heading, tasks) in [
            ("Новые задачи", &self.new_tasks),
            ("Просроченные задачи", &self.overdue_tasks),
        ] {
            text.push_str(&format!("\n## {heading}\n"));
            for task in tasks {
                text.push_str(&format!(
                    "- [{}] {} - {} - {} ({})\n",
                    if task.completed { "x" } else { " " },
                    task.text,
                    task.owner,
                    task.due_text,
                    task.meeting_title
                ));
            }
        }
        text.push_str("\n## По людям\n");
        for (name, lines) in &self.highlights {
            text.push_str(&format!("\n### {name}\n"));
            for line in lines {
                text.push_str(&format!("- {line}\n"));
            }
        }
        text
    }
}

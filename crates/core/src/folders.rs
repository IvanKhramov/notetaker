//! Virtual organization. Meeting directories never move.
use crate::{Library, Meeting, MeetingMeta, Mode, Result, Revision, Versioned};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderSettings {
    pub template: Option<String>,
    pub vocabulary: String,
    pub mode: Option<Mode>,
    pub speaker_hint: Option<u32>,
    pub rules: Vec<CalendarRule>,
    pub tracker: Option<TrackerConfig>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CalendarRule {
    pub title_contains: String,
    pub attendee_domain: String,
}
impl CalendarRule {
    fn matches(&self, event: &crate::calendar::EventLink) -> bool {
        (!self.title_contains.trim().is_empty() || !self.attendee_domain.trim().is_empty())
            && (self.title_contains.trim().is_empty()
                || event
                    .title
                    .to_lowercase()
                    .contains(&self.title_contains.trim().to_lowercase()))
            && (self.attendee_domain.trim().is_empty()
                || event.attendees.iter().any(|a| {
                    a.email.rsplit_once('@').is_some_and(|(_, d)| {
                        d.eq_ignore_ascii_case(self.attendee_domain.trim().trim_start_matches('@'))
                    })
                }))
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerKind {
    #[default]
    Webhook,
    Kaiten,
    Jira,
    YouTrack,
}
/// Shared configuration contains credential identifiers, never secret values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: TrackerKind,
    pub base_url: String,
    pub credential_id: String,
    pub signature_header: String,
    pub email: String,
    pub project: String,
    pub issue_type: String,
    pub jira_v3: bool,
    pub space_id: String,
    pub board_id: String,
    pub column_id: String,
    pub lane_id: String,
    /// Email -> Kaiten user id. No guessed owners.
    pub owners: BTreeMap<String, u64>,
}
pub type Folders = BTreeMap<String, FolderSettings>;

/// None = all; empty string = unassigned; parent includes descendants.
pub fn matches_folder(meta: &MeetingMeta, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some("") => meta.folder.as_deref().is_none_or(str::is_empty),
        Some(folder) => meta.folder.as_deref().is_some_and(|name| {
            name == folder
                || name
                    .strip_prefix(folder)
                    .is_some_and(|tail| tail.starts_with('/'))
        }),
    }
}
impl Library {
    pub fn load_folders(&self) -> Result<Versioned<Folders>> {
        let loaded = self.load_shared_text("folders.json")?;
        Ok(Versioned {
            value: if loaded.value.is_empty() {
                Folders::default()
            } else {
                serde_json::from_str(&loaded.value)?
            },
            revision: loaded.revision,
        })
    }
    pub fn save_folders(&self, folders: &Folders, revision: Revision) -> Result<Revision> {
        self.save_shared_text(
            "folders.json",
            &serde_json::to_string_pretty(folders)?,
            revision,
        )
    }
    /// Rules take priority over latest series assignment and selected folder.
    pub fn inherit_folder(
        &self,
        meta: &mut MeetingMeta,
        selected: Option<&str>,
    ) -> Result<Option<FolderSettings>> {
        let folders = self.load_folders()?.value;
        let rule = meta.calendar_event.as_ref().and_then(|event| {
            folders
                .iter()
                .find(|(_, settings)| settings.rules.iter().any(|r| r.matches(event)))
                .map(|(name, _)| name.clone())
        });
        let mut previous = self
            .list_meetings()?
            .into_iter()
            .filter_map(|m| m.read_meta().ok())
            .filter(|m| m.started_at < meta.started_at && m.folder.is_some())
            .collect::<Vec<_>>();
        previous.sort_by_key(|m| std::cmp::Reverse(m.started_at));
        let series = previous
            .iter()
            .find(|m| match (&meta.calendar_event, &m.calendar_event) {
                (Some(a), Some(b)) => !a.uid.is_empty() && a.uid == b.uid,
                // Unnamed speakers provide no reliable series identity before recording.
                _ => false,
            })
            .and_then(|m| m.folder.clone());
        if meta.folder.is_none() {
            meta.folder = rule
                .or(series)
                .or_else(|| selected.filter(|s| !s.is_empty()).map(str::to_owned));
        }
        let defaults = meta
            .folder
            .as_ref()
            .and_then(|name| folders.get(name))
            .cloned();
        if let Some(settings) = &defaults {
            if meta.template.is_none() {
                meta.template = settings.template.clone();
            }
            if let Some(mode) = settings.mode {
                meta.mode = mode;
            }
        }
        Ok(defaults)
    }
    pub fn assign_folder(&self, meetings: &[Meeting], folder: Option<&str>) -> Result<()> {
        for meeting in meetings {
            if meeting.path().parent() != Some(self.root()) {
                return Err(crate::Error::TaskEdit("Встреча вне библиотеки".into()));
            }
            meeting
                .update_meta(|m| m.folder = folder.filter(|s| !s.is_empty()).map(str::to_owned))?;
        }
        self.reindex_incremental()
    }
    /// Rename includes descendants; deleting a tree only unassigns its meetings.
    pub fn rename_folder(&self, old: &str, new: Option<&str>) -> Result<()> {
        if old.is_empty() {
            return Err(crate::Error::TaskEdit("Пустое имя папки".into()));
        }
        let mut loaded = self.load_folders()?;
        let keys = loaded
            .value
            .keys()
            .filter(|k| *k == old || k.starts_with(&format!("{old}/")))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(new) = new
            && (new.trim().is_empty()
                || loaded
                    .value
                    .keys()
                    .any(|k| k == new || k.starts_with(&format!("{new}/"))))
        {
            return Err(crate::Error::TaskEdit(
                "Папка уже существует или имя пустое".into(),
            ));
        }
        for key in keys {
            let settings = loaded.value.remove(&key).expect("existing folder");
            if let Some(new) = new {
                loaded
                    .value
                    .insert(format!("{new}{}", &key[old.len()..]), settings);
            }
        }
        self.save_folders(&loaded.value, loaded.revision)?;
        for meeting in self.list_meetings()? {
            if matches_folder(&meeting.read_meta()?, Some(old)) {
                meeting.update_meta(|m| {
                    m.folder = new.map(|new| {
                        format!(
                            "{new}{}",
                            &m.folder.as_ref().expect("assigned")[old.len()..]
                        )
                    })
                })?;
            }
        }
        self.reindex_incremental()
    }
    pub fn list_tasks_in_folder(
        &self,
        completed: Option<bool>,
        owner: Option<&str>,
        exclude_local: bool,
        folder: Option<&str>,
    ) -> Result<Vec<crate::Task>> {
        Ok(self
            .list_tasks(completed, owner, exclude_local)?
            .into_iter()
            .filter(|t| {
                t.meeting(self)
                    .and_then(|m| m.read_meta())
                    .is_ok_and(|m| matches_folder(&m, folder))
            })
            .collect())
    }
    pub fn people_in_folder(
        &self,
        exclude_local: bool,
        user_name: &str,
        user_aliases: &str,
        folder: Option<&str>,
    ) -> Result<Vec<crate::Person>> {
        let people = self.people(exclude_local, user_name, user_aliases)?;
        if folder.is_none() {
            return Ok(people);
        }
        let meetings = self
            .meeting_summaries(exclude_local)?
            .into_iter()
            .filter(|m| matches_folder(&m.meta, folder))
            .collect::<Vec<_>>();
        let tasks = self.list_tasks_in_folder(None, None, exclude_local, folder)?;
        Ok(people
            .into_iter()
            .filter(|p| {
                meetings.iter().any(|m| m.has_person(p))
                    || tasks
                        .iter()
                        .any(|t| p.matches(&t.owner) || p.matches(&t.requester))
            })
            .collect())
    }
}

impl Library {
    pub fn series_in_folder(
        &self,
        exclude_local: bool,
        folder: Option<&str>,
    ) -> Result<Vec<crate::Series>> {
        Ok(self
            .series(exclude_local)?
            .into_iter()
            .filter_map(|mut s| {
                s.meetings.retain(|m| matches_folder(&m.meta, folder));
                (!s.meetings.is_empty()).then_some(s)
            })
            .collect())
    }
}

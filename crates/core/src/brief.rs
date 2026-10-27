use crate::{Library, Meeting, Result, calendar::CalendarEvent, person_matches};

impl Library {
    /// Attendee matches take precedence, then title FTS; only earlier meetings.
    pub fn brief_sources(&self, event: &CalendarEvent, limit: usize) -> Result<Vec<Meeting>> {
        self.reindex_incremental()?;
        let title_hits = self.retrieve(&event.link.title, 100, false)?;
        let mut selected = Vec::new();
        for meeting in self.list_meetings()? {
            if meeting.validate_source_paths().is_err() {
                continue;
            }
            let meta = meeting.read_meta()?;
            if meta.started_at >= event.start {
                continue;
            }
            let overlap = meta.calendar_event.as_ref().is_some_and(|previous| {
                previous.attendees.iter().any(|a| {
                    event.link.attendees.iter().any(|b| {
                        person_matches(&a.email, &b.email) || person_matches(&a.name, &b.name)
                    })
                })
            });
            let title_rank = title_hits.iter().position(|hit| hit.path == meeting.path());
            if overlap || title_rank.is_some() {
                selected.push((
                    !overlap,
                    std::cmp::Reverse(meta.started_at),
                    title_rank,
                    meeting,
                ));
            }
        }
        selected.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
        Ok(selected
            .into_iter()
            .take(limit.min(20))
            .map(|(_, _, _, meeting)| meeting)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MeetingMeta,
        calendar::{Attendee, EventLink},
    };
    #[test]
    fn attendees_or_title_only_earlier_sources() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path()).unwrap();
        let now = chrono::Utc::now();
        let link = EventLink {
            uid: "event".into(),
            title: "Кластер".into(),
            attendees: vec![Attendee {
                name: "Иван".into(),
                email: "ivan@example.org".into(),
            }],
            call_url: None,
        };
        let mut meta = MeetingMeta::new("Иное", now - chrono::Duration::days(1));
        meta.calendar_event = Some(link.clone());
        let overlap = lib.create_meeting(meta).unwrap();
        let title = lib
            .create_meeting(MeetingMeta::new("Кластер", now - chrono::Duration::days(2)))
            .unwrap();
        lib.create_meeting(MeetingMeta::new("Кластер", now + chrono::Duration::days(1)))
            .unwrap();
        lib.create_meeting(MeetingMeta::new("Погода", now - chrono::Duration::days(1)))
            .unwrap();
        let event = CalendarEvent {
            link,
            start: now,
            end: now + chrono::Duration::hours(1),
            all_day: false,
            organizer: None,
        };
        let sources = lib.brief_sources(&event, 6).unwrap();
        assert_eq!(
            sources.iter().map(Meeting::path).collect::<Vec<_>>(),
            vec![overlap.path(), title.path()]
        );
        assert_eq!(sources[0].read_meta().unwrap().mode, crate::Mode::Local);
    }
}

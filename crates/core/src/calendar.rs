//! Bounded ICS subscription subset. Occurrences retain wall-clock time across DST.
use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub name: String,
    pub email: String,
}
impl Attendee {
    pub fn label(&self) -> &str {
        if self.name.is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventLink {
    pub uid: String,
    pub title: String,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub link: EventLink,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub organizer: Option<Attendee>,
}

/// All-day entries are context, never candidates for an automatic call link.
pub fn overlapping(events: &[CalendarEvent], now: DateTime<Utc>) -> Option<&CalendarEvent> {
    events
        .iter()
        .filter(|event| {
            !event.all_day
                && event.start <= now + Duration::minutes(10)
                && event.end >= now - Duration::minutes(10)
        })
        .min_by_key(|event| {
            (
                (event.start - now).num_seconds().abs(),
                event.start,
                &event.link.uid,
            )
        })
}

pub fn link_meeting(meta: &mut crate::MeetingMeta, event: &CalendarEvent, typed_title: bool) {
    if !typed_title {
        meta.title.clone_from(&event.link.title);
    }
    meta.calendar_event = Some(event.link.clone());
}

#[derive(Debug, thiserror::Error)]
#[error("Некорректный или неподдерживаемый календарь ICS: {0}")]
pub struct CalendarError(&'static str);
type Result<T> = std::result::Result<T, CalendarError>;

#[derive(Clone)]
struct Property {
    name: String,
    params: BTreeMap<String, String>,
    value: String,
}

// Property delimiters inside quoted parameter values do not separate fields.
fn split_quoted(value: &str, delimiter: char) -> Vec<&str> {
    let mut quoted = false;
    value
        .split(|ch| {
            if ch == '"' {
                quoted = !quoted;
            }
            ch == delimiter && !quoted
        })
        .collect()
}
fn property(line: &str) -> Result<Property> {
    let parts = split_quoted(line, ':');
    let header = parts.first().copied().unwrap_or_default();
    let value = line
        .get(header.len() + 1..)
        .ok_or(CalendarError("строка без значения"))?;
    let mut fields = split_quoted(header, ';').into_iter();
    let name = fields.next().unwrap_or_default().to_ascii_uppercase();
    let mut params = BTreeMap::new();
    for field in fields {
        let (key, value) = field.split_once('=').ok_or(CalendarError("параметр"))?;
        params.insert(
            key.to_ascii_uppercase(),
            value
                .trim_matches('"')
                .replace("^n", "\n")
                .replace("^'", "\"")
                .replace("^^", "^"),
        );
    }
    Ok(Property {
        name,
        params,
        value: value.into(),
    })
}
fn unescape(text: &str) -> String {
    let mut chars = text.chars();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(ch) => out.push(ch),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// IANA TZIDs plus common Outlook Windows IDs. Unknown zones fail explicitly.
pub fn timezone(id: &str) -> Result<Tz> {
    let id = match id {
        "Russian Standard Time" => "Europe/Moscow",
        "GMT Standard Time" => "Europe/London",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "UTC" => "Etc/UTC",
        other => other,
    };
    id.parse().map_err(|_| CalendarError("неизвестный TZID"))
}
#[derive(Clone)]
struct Date {
    wall: NaiveDateTime,
    zone: Tz,
    all_day: bool,
}
impl Date {
    fn utc(&self) -> Result<DateTime<Utc>> {
        wall_utc(self.zone, self.wall)
    }
}
fn wall_utc(zone: Tz, wall: NaiveDateTime) -> Result<DateTime<Utc>> {
    // Fall-back ambiguity uses the first occurrence; nonexistent spring times fail.
    zone.from_local_datetime(&wall)
        .earliest()
        .map(|date| date.with_timezone(&Utc))
        .ok_or(CalendarError("время попало в пропуск DST"))
}
fn date(value: &str, params: &BTreeMap<String, String>, fallback: Tz) -> Result<Date> {
    let all_day = params.get("VALUE").is_some_and(|v| v == "DATE") || value.len() == 8;
    let zone = if value.ends_with('Z') {
        chrono_tz::UTC
    } else {
        params
            .get("TZID")
            .map(|id| timezone(id))
            .transpose()?
            .unwrap_or(fallback)
    };
    let wall = if all_day {
        NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
    } else {
        NaiveDateTime::parse_from_str(value.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()
    }
    .ok_or(CalendarError("дата"))?;
    Ok(Date {
        wall,
        zone,
        all_day,
    })
}
fn attendee(p: &Property) -> Attendee {
    Attendee {
        name: unescape(p.params.get("CN").map(String::as_str).unwrap_or_default()),
        email: p
            .value
            .get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
            .map_or_else(|| p.value.clone(), |_| p.value[7..].to_owned()),
    }
}

/// Only known call hosts and HTTP(S); suffix lookalikes and userinfo are rejected.
pub fn call_url(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
        .filter_map(|part| {
            let offset = part.find("https://").or_else(|| part.find("http://"))?;
            let url = part[offset..].trim_end_matches([',', '.', ';', ')', ']', '}']);
            let authority = url.split_once("://")?.1.split(['/', '?', '#']).next()?;
            let host = authority.to_ascii_lowercase();
            [
                "ktalk.ru",
                "telemost.yandex.ru",
                "meet.google.com",
                "zoom.us",
            ]
            .iter()
            .any(|allowed| {
                host == *allowed || (*allowed == "zoom.us" && host.ends_with(".zoom.us"))
            })
            .then(|| url.to_owned())
        })
        .next()
}

/// Expand only the requested window, with a hard work limit for untrusted feeds.
pub fn parse_ics(
    text: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    fallback: Tz,
) -> Result<Vec<CalendarEvent>> {
    if text.len() > 8 * 1024 * 1024 {
        return Err(CalendarError("файл больше 8 МБ"));
    }
    if to < from || to - from > Duration::days(32) {
        return Err(CalendarError("окно больше 32 дней"));
    }
    let mut lines: Vec<String> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        if line.starts_with([' ', '\t']) {
            lines
                .last_mut()
                .ok_or(CalendarError("перенос строки"))?
                .push_str(&line[1..]);
        } else {
            lines.push(line.trim_end_matches('\r').into());
        }
    }
    if !lines.iter().any(|line| line == "BEGIN:VCALENDAR")
        || !lines.iter().any(|line| line == "END:VCALENDAR")
    {
        return Err(CalendarError("нет VCALENDAR"));
    }
    let mut result = Vec::new();
    let mut props = Vec::new();
    let mut components = Vec::new();
    let mut depth = 0usize;
    let mut feed_zone = fallback;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line == "BEGIN:VEVENT" {
            props.clear();
            depth = 1;
        } else if line == "END:VEVENT" {
            if depth != 1 {
                return Err(CalendarError("структура VEVENT"));
            }
            components.push(std::mem::take(&mut props));
            if components.len() > 10_000 {
                return Err(CalendarError("слишком много VEVENT"));
            }
            depth = 0;
        } else if depth > 0 {
            if line.starts_with("BEGIN:") {
                depth += 1;
            } else if line.starts_with("END:") {
                depth = depth.saturating_sub(1);
            } else if depth == 1 {
                props.push(property(&line)?);
            }
        } else if let Some(id) = line.strip_prefix("X-WR-TIMEZONE:") {
            feed_zone = timezone(id)?;
        }
    }
    if depth != 0 {
        return Err(CalendarError("незавершённый VEVENT"));
    }
    let mut budget = 1_000_000usize;
    for props in components
        .iter()
        .filter(|props| !props.iter().any(|p| p.name == "RECURRENCE-ID"))
    {
        expand(props, from, to, feed_zone, &mut result, &mut budget)?;
        if result.len() > 10_000 {
            return Err(CalendarError("слишком много событий"));
        }
    }
    // Detached changes/cancellations replace a single original occurrence.
    for props in components
        .iter()
        .filter(|props| props.iter().any(|p| p.name == "RECURRENCE-ID"))
    {
        let uid = props
            .iter()
            .find(|p| p.name == "UID")
            .ok_or(CalendarError("нет UID"))?;
        let recurrence = props
            .iter()
            .find(|p| p.name == "RECURRENCE-ID")
            .expect("override");
        if recurrence.params.contains_key("RANGE") {
            return Err(CalendarError("RANGE в RECURRENCE-ID"));
        }
        let original = date(&recurrence.value, &recurrence.params, feed_zone)?.utc()?;
        result.retain(|event| event.link.uid != uid.value || event.start != original);
        expand(props, from, to, feed_zone, &mut result, &mut budget)?;
        if result.len() > 10_000 {
            return Err(CalendarError("слишком много событий"));
        }
    }
    result.sort_by_key(|e| (e.start, e.link.uid.clone()));
    result.dedup_by(|a, b| a.start == b.start && a.link.uid == b.link.uid);
    Ok(result)
}

fn expand(
    props: &[Property],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    fallback: Tz,
    out: &mut Vec<CalendarEvent>,
    budget: &mut usize,
) -> Result<()> {
    let get = |name: &str| props.iter().find(|p| p.name == name);
    if get("STATUS").is_some_and(|p| p.value == "CANCELLED") {
        return Ok(());
    }
    let start = get("DTSTART").ok_or(CalendarError("нет DTSTART"))?;
    let start = date(&start.value, &start.params, fallback)?;
    let end = get("DTEND")
        .map(|p| date(&p.value, &p.params, start.zone))
        .transpose()?;
    let duration = match &end {
        Some(end) => end.utc()? - start.utc()?,
        None if start.all_day => Duration::days(1),
        None => Duration::hours(1),
    };
    if duration <= Duration::zero() {
        return Err(CalendarError("DTEND раньше DTSTART"));
    }
    let title = get("SUMMARY")
        .map(|p| unescape(&p.value))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Встреча".into());
    let uid = get("UID").ok_or(CalendarError("нет UID"))?.value.clone();
    let attendees = props
        .iter()
        .filter(|p| p.name == "ATTENDEE")
        .map(attendee)
        .collect();
    let organizer = get("ORGANIZER").map(attendee);
    let url = ["URL", "LOCATION", "DESCRIPTION"]
        .into_iter()
        .filter_map(get)
        .find_map(|p| call_url(&unescape(&p.value)));
    let link = EventLink {
        uid,
        title,
        attendees,
        call_url: url,
    };
    let mut excluded = Vec::new();
    for p in props.iter().filter(|p| p.name == "EXDATE") {
        for value in p.value.split(',') {
            excluded.push(date(value, &p.params, start.zone)?.utc()?);
        }
    }
    let mut add = |wall| -> Result<()> {
        let begin = match wall_utc(start.zone, wall) {
            Ok(begin) => begin,
            Err(_) => return Ok(()),
        };
        let finish = if start.all_day {
            wall_utc(
                start.zone,
                wall + end
                    .as_ref()
                    .map_or(Duration::days(1), |end| end.wall - start.wall),
            )?
        } else {
            begin + duration
        };
        if begin <= to && finish >= from && !excluded.contains(&begin) {
            out.push(CalendarEvent {
                link: link.clone(),
                start: begin,
                end: finish,
                all_day: start.all_day,
                organizer: organizer.clone(),
            });
        }
        Ok(())
    };
    let Some(rule) = get("RRULE") else {
        return add(start.wall);
    };
    let mut fields = BTreeMap::new();
    for part in rule.value.split(';') {
        let (key, value) = part.split_once('=').ok_or(CalendarError("RRULE"))?;
        if !["FREQ", "COUNT", "UNTIL", "BYDAY", "INTERVAL", "WKST"].contains(&key) {
            return Err(CalendarError("параметр RRULE"));
        }
        fields.insert(key, value);
    }
    let frequency = *fields.get("FREQ").ok_or(CalendarError("нет FREQ"))?;
    if !["DAILY", "WEEKLY"].contains(&frequency) {
        return Err(CalendarError("FREQ: поддержаны DAILY/WEEKLY"));
    }
    let number = |key, default| -> Result<usize> {
        fields.get(key).map_or(Ok(default), |value| {
            value
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(CalendarError("число RRULE"))
        })
    };
    let interval = number("INTERVAL", 1)?;
    let count = number("COUNT", usize::MAX)?;
    let until = fields
        .get("UNTIL")
        .map(|v| date(v, &BTreeMap::new(), start.zone).and_then(|d| d.utc()))
        .transpose()?;
    let weekdays = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
    let week_start = fields.get("WKST").map_or(Ok(0), |v| {
        weekdays
            .iter()
            .position(|day| day == v)
            .ok_or(CalendarError("WKST"))
    })?;
    let byday = fields
        .get("BYDAY")
        .map(|v| {
            v.split(',')
                .map(|v| {
                    weekdays
                        .iter()
                        .position(|day| *day == v)
                        .ok_or(CalendarError("BYDAY"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let first_weekday = start.wall.weekday().num_days_from_monday() as usize;
    let mut emitted = 0;
    for day in 0..100_000usize {
        *budget = budget
            .checked_sub(1)
            .ok_or(CalendarError("превышен лимит расчёта повторов"))?;
        let wall = start
            .wall
            .checked_add_signed(Duration::days(day as i64))
            .ok_or(CalendarError("дата вне диапазона"))?;
        let Some(begin) = start
            .zone
            .from_local_datetime(&wall)
            .earliest()
            .map(|d| d.with_timezone(&Utc))
        else {
            continue;
        };
        if begin > to || until.is_some_and(|u| begin > u) {
            return Ok(());
        }
        let weekday = wall.weekday().num_days_from_monday() as usize;
        let selected = if frequency == "DAILY" {
            day.is_multiple_of(interval)
        } else {
            ((day + (first_weekday + 7 - week_start) % 7) / 7).is_multiple_of(interval)
        };
        let selected = selected
            && byday
                .as_ref()
                .map_or(frequency == "DAILY" || weekday == first_weekday, |days| {
                    days.contains(&weekday)
                });
        if selected || day == 0 {
            emitted += 1;
            add(wall)?;
            if emitted >= count {
                return Ok(());
            }
        }
    }
    Err(CalendarError("повтор длиннее 100000 дней"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }
    const YANDEX: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test Yandex-style fixture//RU\r\nX-WR-TIMEZONE:Europe/Moscow\r\nBEGIN:VEVENT\r\nUID:team-1\r\nDTSTART;TZID=Europe/Moscow:20261026T120000\r\nDTEND;TZID=Europe/Moscow:20261026T130000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=5\r\nEXDATE;TZID=Europe/Moscow:20261028T120000\r\nSUMMARY:Планирование\\, команда\r\nATTENDEE;CN=\"Иван Петров\":mailto:ivan@example.org\r\nORGANIZER;CN=Ольга:mailto:olga@example.org\r\nDESCRIPTION:Звонок https://telemost.yandex.ru/j/\r\n 123\\nПовестка\r\nBEGIN:VALARM\r\nDESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:day\r\nDTSTART;VALUE=DATE:20261027\r\nDTEND;VALUE=DATE:20261028\r\nSUMMARY:Отпуск\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    #[test]
    fn yandex_moscow_all_day_weekly_exdate_and_linking() {
        let events = parse_ics(
            YANDEX,
            utc("2026-10-26T00:00:00Z"),
            utc("2026-11-12T00:00:00Z"),
            chrono_tz::UTC,
        )
        .unwrap();
        assert_eq!(events.len(), 5);
        let event = &events[0];
        assert_eq!(event.start, utc("2026-10-26T09:00:00Z"));
        assert_eq!(event.link.title, "Планирование, команда");
        assert_eq!(event.link.attendees[0].label(), "Иван Петров");
        assert_eq!(event.link.attendees[0].email, "ivan@example.org");
        assert_eq!(event.organizer.as_ref().unwrap().name, "Ольга");
        assert_eq!(
            event.link.call_url.as_deref(),
            Some("https://telemost.yandex.ru/j/123")
        );
        assert!(events[1].all_day);
        assert_eq!(events[1].end - events[1].start, Duration::days(1));
        assert!(
            !events
                .iter()
                .any(|e| e.start == utc("2026-10-28T09:00:00Z"))
        );
        assert!(overlapping(&events, utc("2026-10-26T08:50:00Z")).is_some());
        assert!(overlapping(&events, utc("2026-10-26T08:49:59Z")).is_none());
        assert!(overlapping(&events, utc("2026-10-27T10:00:00Z")).is_none());
        let mut meta = crate::MeetingMeta::new("typed", event.start);
        link_meeting(&mut meta, event, true);
        assert_eq!(meta.title, "typed");
        link_meeting(&mut meta, event, false);
        assert_eq!(meta.title, event.link.title);
        assert_eq!(meta.calendar_event.as_ref().unwrap().uid, "team-1");
        let yaml = serde_yaml::to_string(&meta).unwrap();
        assert_eq!(
            serde_yaml::from_str::<crate::MeetingMeta>(&yaml).unwrap(),
            meta
        );
        meta.calendar_event = None;
        assert!(
            serde_yaml::from_str::<crate::MeetingMeta>(&serde_yaml::to_string(&meta).unwrap())
                .unwrap()
                .calendar_event
                .is_none()
        );
    }
    #[test]
    fn daily_until_dst_and_windows_zone() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:dst\nDTSTART;TZID=Eastern Standard Time:20261031T100000\nDTEND;TZID=Eastern Standard Time:20261031T110000\nRRULE:FREQ=DAILY;UNTIL=20261102T150000Z\nEND:VEVENT\nEND:VCALENDAR";
        let events = parse_ics(
            text,
            utc("2026-10-30T00:00:00Z"),
            utc("2026-11-10T00:00:00Z"),
            chrono_tz::UTC,
        )
        .unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].start, utc("2026-10-31T14:00:00Z"));
        assert_eq!(events[1].start, utc("2026-11-01T15:00:00Z"));
        assert_eq!(events[2].start, utc("2026-11-02T15:00:00Z"));
        assert_eq!(call_url("https://zoom.us.evil.org/j/1"), None);
        assert_eq!(call_url("https://zoom.us@evil.org/j/1"), None);
        assert_eq!(
            call_url("https://us02web.zoom.us/j/1"),
            Some("https://us02web.zoom.us/j/1".into())
        );
    }
}

#[cfg(test)]
mod recurrence_tests {
    use super::*;
    #[test]
    fn modified_and_cancelled_occurrences_replace_the_original() {
        let text = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:x\nDTSTART:20261026T100000Z\nDTEND:20261026T110000Z\nRRULE:FREQ=WEEKLY;COUNT=3;BYDAY=MO\nSUMMARY:Regular\nEND:VEVENT\nBEGIN:VEVENT\nUID:x\nRECURRENCE-ID:20261102T100000Z\nDTSTART:20261102T120000Z\nDTEND:20261102T130000Z\nSUMMARY:Moved\nEND:VEVENT\nBEGIN:VEVENT\nUID:x\nRECURRENCE-ID:20261109T100000Z\nSTATUS:CANCELLED\nEND:VEVENT\nEND:VCALENDAR";
        let start = DateTime::parse_from_rfc3339("2026-10-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let events = parse_ics(text, start, start + Duration::days(20), chrono_tz::UTC).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].link.title, "Moved");
        assert_eq!(events[1].start.format("%H:%M").to_string(), "12:00");
        assert!(
            parse_ics(
                &text.replace("FREQ=WEEKLY", "FREQ=MONTHLY"),
                start,
                start + Duration::days(20),
                chrono_tz::UTC
            )
            .is_err()
        );
        assert!(parse_ics("", start, start, chrono_tz::UTC).is_err());
        assert!(timezone("Unknown Standard Time").is_err());
    }
}

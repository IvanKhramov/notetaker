//! Bounded ICS subscription subset. Occurrences retain wall-clock time across DST.
use std::collections::{BTreeMap, BTreeSet};
mod recurrence;

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
type Expansion = (Vec<CalendarEvent>, Option<(String, DateTime<Utc>)>);

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
const EVENT_LIMIT: usize = 10_000;
const FEED_LIMIT: usize = 10_000;
const EVENT_BYTES: usize = 8 * 1024 * 1024;
const FEED_BYTES: usize = 32 * 1024 * 1024;
const FEED_LIMIT_ERROR: &str = "превышен общий лимит 10000 вхождений календаря";
const FEED_BYTES_ERROR: &str = "превышен общий лимит памяти календаря (32 МиБ)";
#[derive(Clone, Copy, Default)]
struct FeedUsage {
    count: usize,
    bytes: usize,
}
fn clean_text(text: &str, cap: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(cap)
        .collect::<String>()
        .trim()
        .to_owned()
}
fn bounded_id(value: &str) -> Result<String> {
    let id = clean_text(value, 1025);
    if id.chars().count() > 1024 {
        return Err(CalendarError("лимит длины UID"));
    }
    Ok(id)
}
fn event_bytes(event: &CalendarEvent) -> usize {
    let attendee_bytes = |a: &Attendee| a.name.len() + a.email.len();
    std::mem::size_of::<CalendarEvent>()
        + event.link.uid.len()
        + event.link.title.len()
        + event.link.call_url.as_ref().map_or(0, String::len)
        + event
            .link
            .attendees
            .iter()
            .map(|a| std::mem::size_of::<Attendee>() + attendee_bytes(a))
            .sum::<usize>()
        + event.organizer.as_ref().map_or(0, attendee_bytes)
}
fn attendee(p: &Property) -> Attendee {
    Attendee {
        name: clean_text(
            &unescape(p.params.get("CN").map(String::as_str).unwrap_or_default()),
            200,
        ),
        email: clean_text(
            &p.value
                .get(..7)
                .filter(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
                .map_or_else(|| p.value.clone(), |_| p.value[7..].to_owned()),
            320,
        ),
    }
}

/// Decide trust before a URL can be displayed as a call link or opened.
pub fn is_allowed_call_link(value: &str) -> bool {
    if value
        .chars()
        .any(|ch| ch == '\\' || ch.is_control() || ch.is_whitespace())
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some_and(|port| port != 443)
    {
        return false;
    }
    // Check the original authority too: URL parsing normalises IDNs and escapes.
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') || authority.ends_with(':') {
        return false;
    }
    let raw_host = authority.split(':').next().unwrap_or_default();
    if raw_host.is_empty()
        || !raw_host.is_ascii()
        || !raw_host
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'.')
    {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    let raw_host = raw_host.to_ascii_lowercase();
    let raw_host = raw_host.strip_suffix('.').unwrap_or(raw_host.as_str());
    if host != raw_host {
        return false;
    }
    if !host.is_ascii()
        || host
            .split('.')
            .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
    {
        return false;
    }
    matches!(
        host,
        "ktalk.ru" | "telemost.yandex.ru" | "meet.google.com" | "zoom.us"
    ) || host.ends_with(".zoom.us")
}

/// Extract a known HTTPS call link from calendar URL, LOCATION or DESCRIPTION text.
pub fn call_url(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
        .filter_map(|part| {
            let offset = part.to_ascii_lowercase().find("https://")?;
            let value = part[offset..].trim_end_matches([',', '.', ';', ')', ']', '}']);
            is_allowed_call_link(value).then(|| value.to_owned())
        })
        .next()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    Recurrence,
    Timezone,
    Date,
    Format,
    Limit,
}
impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recurrence => "неподдерживаемое повторение",
            Self::Timezone => "часовой пояс",
            Self::Date => "дата",
            Self::Format => "формат события",
            Self::Limit => "лимит обработки",
        }
    }
}
#[derive(Debug, Clone)]
pub struct SkippedEvent {
    pub title: String,
    pub reason: SkipReason,
}
#[derive(Debug, Clone, Default)]
pub struct ParsedCalendar {
    pub events: Vec<CalendarEvent>,
    pub skipped: Vec<SkippedEvent>,
}
impl CalendarError {
    fn reason(&self) -> SkipReason {
        if self.0.contains("TZID") || self.0.contains("DST") {
            SkipReason::Timezone
        } else if self.0.contains("лимит")
            || self.0.contains("100000")
            || self.0.contains("слишком много")
        {
            SkipReason::Limit
        } else if ["RRULE", "FREQ", "BY", "WKST", "RANGE", "RDATE", "DURATION"]
            .iter()
            .any(|v| self.0.contains(v))
        {
            SkipReason::Recurrence
        } else if self.0.contains("дата") || self.0.contains("DTSTART") || self.0.contains("DTEND")
        {
            SkipReason::Date
        } else {
            SkipReason::Format
        }
    }
}
/// Read-only compatibility entry; the parser always produces event diagnostics.
pub fn parse_ics(
    text: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    fallback: Tz,
) -> Result<Vec<CalendarEvent>> {
    Ok(parse_ics_report(text, from, to, fallback)?.events)
}
/// Expand only the requested window, with a hard work limit for untrusted feeds.
pub fn parse_ics_report(
    text: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    fallback: Tz,
) -> Result<ParsedCalendar> {
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
    let mut result = ParsedCalendar::default();
    let mut event_error = None;
    let mut props = Vec::new();
    let mut components = Vec::new();
    let mut depth = 0usize;
    let mut feed_zone = Ok(fallback);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line == "BEGIN:VEVENT" {
            if depth > 0 {
                components.push((
                    std::mem::take(&mut props),
                    Some(CalendarError("структура VEVENT")),
                ));
            }
            props.clear();
            event_error = None;
            depth = 1;
        } else if line == "END:VEVENT" {
            if depth != 1 {
                event_error = Some(CalendarError("структура VEVENT"));
            }
            if components.len() >= 10_000 {
                return Err(CalendarError("превышен общий лимит 10000 VEVENT"));
            }
            components.push((std::mem::take(&mut props), event_error.take()));
            depth = 0;
        } else if line == "END:VCALENDAR" && depth > 0 {
            components.push((
                std::mem::take(&mut props),
                Some(CalendarError("незавершённый VEVENT")),
            ));
            depth = 0;
        } else if depth > 0 {
            if line.starts_with("BEGIN:") {
                depth += 1;
            } else if line.starts_with("END:") {
                depth = depth.saturating_sub(1);
            } else if depth == 1 {
                match property(&line) {
                    Ok(p) => props.push(p),
                    Err(error) => event_error = Some(error),
                }
            }
        } else if let Some(id) = line.strip_prefix("X-WR-TIMEZONE:") {
            feed_zone = timezone(id);
        }
    }
    if depth != 0 {
        components.push((props, Some(CalendarError("незавершённый VEVENT"))));
    }
    let mut usage = FeedUsage::default();
    // Each component is transactional: failed expansion never leaves partial events.
    for detached in [false, true] {
        for (props, error) in &components {
            let recurrence = props.iter().find(|p| p.name == "RECURRENCE-ID");
            if recurrence.is_some() != detached {
                continue;
            }
            let attempt = || -> Result<Expansion> {
                if let Some(error) = error {
                    return Err(CalendarError(error.0));
                }
                let zone = match &feed_zone {
                    Ok(zone) => *zone,
                    Err(error) => {
                        if props.iter().any(|p| {
                            ["DTSTART", "RECURRENCE-ID"].contains(&p.name.as_str())
                                && !p.value.ends_with('Z')
                                && !p.params.contains_key("TZID")
                        }) {
                            return Err(CalendarError(error.0));
                        }
                        fallback
                    }
                };
                let original = if let Some(recurrence) = recurrence {
                    if recurrence.params.contains_key("RANGE") {
                        return Err(CalendarError("RANGE в RECURRENCE-ID"));
                    }
                    let uid = props
                        .iter()
                        .find(|p| p.name == "UID")
                        .ok_or(CalendarError("нет UID"))?;
                    Some((
                        uid.value.clone(),
                        date(&recurrence.value, &recurrence.params, zone)?.utc()?,
                    ))
                } else {
                    None
                };
                let mut events = Vec::new();
                let mut available = usage;
                if let Some((uid, start)) = &original {
                    for event in result
                        .events
                        .iter()
                        .filter(|e| &e.link.uid == uid && e.start == *start)
                    {
                        available.count -= 1;
                        available.bytes -= event_bytes(event);
                    }
                }
                expand(props, from, to, zone, &mut events, available)?;
                Ok((events, original))
            };
            match attempt() {
                Ok((events, original)) => {
                    if let Some((uid, start)) = original {
                        result.events.retain(|event| {
                            let keep = event.link.uid != uid || event.start != start;
                            if !keep {
                                usage.count -= 1;
                                usage.bytes -= event_bytes(event);
                            }
                            keep
                        });
                    }
                    usage.count += events.len();
                    usage.bytes += events.iter().map(event_bytes).sum::<usize>();
                    result.events.extend(events);
                }
                Err(error) if [FEED_LIMIT_ERROR, FEED_BYTES_ERROR].contains(&error.0) => {
                    return Err(error);
                }
                Err(error) => result.skipped.push(SkippedEvent {
                    title: props
                        .iter()
                        .find(|p| p.name == "SUMMARY")
                        .map(|p| clean_text(&unescape(&p.value), 500))
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "Без названия".into()),
                    reason: error.reason(),
                }),
            }
        }
    }
    result.events.sort_by_key(|e| (e.start, e.link.uid.clone()));
    result
        .events
        .dedup_by(|a, b| a.start == b.start && a.link.uid == b.link.uid);
    Ok(result)
}

fn expand(
    props: &[Property],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    fallback: Tz,
    out: &mut Vec<CalendarEvent>,
    usage: FeedUsage,
) -> Result<()> {
    let mut budget = 100_000usize;
    let get = |name: &str| props.iter().find(|p| p.name == name);
    if get("STATUS").is_some_and(|p| p.value == "CANCELLED") {
        return Ok(());
    }
    if get("DURATION").is_some() {
        return Err(CalendarError("DURATION не поддержан"));
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
        .map(|p| clean_text(&unescape(&p.value), 500))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Встреча".into());
    let uid = bounded_id(&get("UID").ok_or(CalendarError("нет UID"))?.value)?;
    let attendees: Vec<_> = props
        .iter()
        .filter(|p| p.name == "ATTENDEE")
        .take(257)
        .map(attendee)
        .collect();
    if attendees.len() > 256 {
        return Err(CalendarError("лимит участников события"));
    }
    let organizer = get("ORGANIZER").map(attendee);
    let url = ["URL", "LOCATION", "DESCRIPTION"]
        .into_iter()
        .filter_map(get)
        .find_map(|p| call_url(&unescape(&p.value).chars().take(16_384).collect::<String>()));
    if url.as_ref().is_some_and(|url| url.len() > 2048) {
        return Err(CalendarError("лимит длины ссылки звонка"));
    }
    let link = EventLink {
        uid,
        title,
        attendees,
        call_url: url,
    };
    let mut excluded = BTreeSet::new();
    for p in props.iter().filter(|p| p.name == "EXDATE") {
        for value in p.value.split(',') {
            excluded.insert(date(value, &p.params, start.zone)?.utc()?);
        }
    }
    let template = CalendarEvent {
        link,
        start: start.utc()?,
        end: start.utc()? + duration,
        all_day: start.all_day,
        organizer,
    };
    let entry_bytes = event_bytes(&template);
    let mut seen = BTreeSet::new();
    let mut add = |begin: DateTime<Utc>, wall: NaiveDateTime| -> Result<()> {
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
            if !seen.insert(begin) {
                return Ok(());
            }
            let bytes = entry_bytes
                .checked_mul(out.len() + 1)
                .ok_or(CalendarError("лимит памяти события"))?;
            if out.len() >= EVENT_LIMIT || bytes > EVENT_BYTES {
                return Err(CalendarError("превышен лимит вхождений или памяти события"));
            }
            if usage.count + out.len() >= FEED_LIMIT {
                return Err(CalendarError(FEED_LIMIT_ERROR));
            }
            if usage.bytes + bytes > FEED_BYTES {
                return Err(CalendarError(FEED_BYTES_ERROR));
            }
            let mut event = template.clone();
            event.start = begin;
            event.end = finish;
            out.push(event);
        }
        Ok(())
    };
    for p in props.iter().filter(|p| p.name == "RDATE") {
        if p.params.get("VALUE").is_some_and(|v| v == "PERIOD") {
            return Err(CalendarError("RDATE PERIOD не поддержан"));
        }
        for value in p.value.split(',') {
            budget = budget
                .checked_sub(1)
                .ok_or(CalendarError("превышен лимит RDATE"))?;
            let extra = date(value, &p.params, start.zone)?.utc()?;
            add(extra, extra.with_timezone(&start.zone).naive_local())?;
        }
    }
    // Resolve recurring wall times once; explicit UTC RDATEs remain absolute.
    let mut emit = |wall| match wall_utc(start.zone, wall) {
        Ok(begin) => add(begin, wall),
        Err(_) => Ok(()),
    };
    if let Some(rule) = get("RRULE") {
        recurrence::expand(
            &rule.value,
            &start,
            from - duration
                - if start.all_day {
                    Duration::days(1)
                } else {
                    Duration::zero()
                },
            to,
            &mut budget,
            emit,
        )
    } else {
        emit(start.wall)
    }
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
    fn call_link_allowlist_parses_urls_before_trusting_hosts() {
        for (value, allowed) in [
            (r"https://evil.example\.zoom.us/j/1", false),
            ("http://zoom.us/j/1", false),
            ("https://zoom.us@evil.example/", false),
            ("https://evil.example/?x=zoom.us", false),
            ("https://notzoom.us/", false),
            ("https://zoom.us.evil.example/", false),
            ("https://ZOOM.US./j/1", true),
            ("https://us02web.zoom.us/j/1", true),
            ("https://sub.meet.google.com/", false),
            ("https://ｚｏｏｍ.us/", false),
            ("https://xn--zm-6kcb.us/", false),
            ("https://zoom.us:8443/j/1", false),
            ("https://zoom.us:443/j/1", true),
            ("https://@zoom.us/", false),
            ("https://zoom.us:/", false),
            ("https:////zoom.us/j/1", false),
            ("https://%7aoom.us/", false),
            ("https://zoom.us/j/1\n", false),
            ("https://zoom.us/j/ 1", false),
            ("https://zoom.us/j/1\u{7f}", false),
            ("https://telemost.yandex.ru/j/1", true),
            ("https://ktalk.ru/j/1", true),
            ("https://meet.google.com/example", true),
        ] {
            assert_eq!(is_allowed_call_link(value), allowed, "{value:?}");
        }
        assert_eq!(
            call_url("Встречаемся (https://zoom.us/j/1)."),
            Some("https://zoom.us/j/1".into())
        );
        let description = r"https://evil.example\\.zoom.us/j/1";
        assert!(call_url(&unescape(description)).is_none());
        let text = format!(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:unsafe\nDTSTART:20261115T100000Z\nDTEND:20261115T110000Z\nDESCRIPTION:{description}\nEND:VEVENT\nEND:VCALENDAR"
        );
        let events = parse_ics(
            &text,
            utc("2026-11-15T00:00:00Z"),
            utc("2026-11-16T00:00:00Z"),
            chrono_tz::UTC,
        )
        .unwrap();
        assert!(events[0].link.call_url.is_none());
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
                &text.replace("FREQ=WEEKLY", "FREQ=HOURLY"),
                start,
                start + Duration::days(20),
                chrono_tz::UTC
            )
            .is_ok()
        );
        assert!(parse_ics("", start, start, chrono_tz::UTC).is_err());
        assert!(timezone("Unknown Standard Time").is_err());
    }
}

#[cfg(test)]
mod recurrence_tests_extended;

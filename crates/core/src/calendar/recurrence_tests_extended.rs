use super::*;
fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}
fn event(extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Neutral Outlook-style fixture//EN\r\nBEGIN:VEVENT\r\nUID:neutral-series\r\nSUMMARY:Планирование проекта\r\n{extra}\r\nEND:VEVENT\r\nEND:VCALENDAR"
    )
}
fn gather(text: &str, first: &str, months: u32) -> Vec<CalendarEvent> {
    let mut from = utc(first);
    let mut result = Vec::new();
    for _ in 0..months {
        let next = from.checked_add_months(chrono::Months::new(1)).unwrap();
        let report =
            parse_ics_report(text, from, next - Duration::seconds(1), chrono_tz::UTC).unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        result.extend(report.events);
        from = next;
    }
    result
}
fn days(events: &[CalendarEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| e.start.format("%Y-%m-%d").to_string())
        .collect()
}
#[test]
fn unsupported_event_does_not_hide_three_good_events_or_expose_properties() {
    let text = "BEGIN:VCALENDAR\nX-WR-TIMEZONE:Europe/Moscow\nBEGIN:VEVENT\nUID:bad\nSUMMARY:Частый повтор\nDTSTART:20261115T100000\nRRULE:FREQ=HOURLY\nX-PRIVATE:https://secret.example.invalid/token\nEND:VEVENT\nBEGIN:VEVENT\nUID:a\nSUMMARY:План\nDTSTART:20261115T100000\nEND:VEVENT\nBEGIN:VEVENT\nUID:b\nSUMMARY:Обзор\nDTSTART:20261116T100000\nRRULE:FREQ=MONTHLY\nEND:VEVENT\nBEGIN:VEVENT\nUID:c\nSUMMARY:Итоги\nDTSTART:20261117T100000\nRRULE:FREQ=YEARLY\nEND:VEVENT\nEND:VCALENDAR";
    let from = utc("2026-11-14T00:00:00Z");
    let report = parse_ics_report(text, from, from + Duration::days(5), chrono_tz::UTC).unwrap();
    assert_eq!(report.events.len(), 3);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].title, "Частый повтор");
    assert_eq!(report.skipped[0].reason, SkipReason::Recurrence);
    assert!(!format!("{report:?}").contains("secret.example"));
    for bad in [
        "DTSTART;TZID=Unknown Standard Time:20261115T100000",
        "DTSTART:not-a-date",
    ] {
        let text = text.replace("DTSTART:20261115T100000\nRRULE:FREQ=HOURLY", bad);
        let report =
            parse_ics_report(&text, from, from + Duration::days(5), chrono_tz::UTC).unwrap();
        assert_eq!(report.events.len(), 3);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            report.skipped[0].reason,
            if bad.contains("TZID") {
                SkipReason::Timezone
            } else {
                SkipReason::Date
            }
        );
    }
    assert!(parse_ics_report("<html>Not a calendar</html>", from, from, chrono_tz::UTC).is_err());
}
#[test]
fn monthly_monthdays_defaults_negative_and_invalid_months_do_not_consume_count() {
    for (rule, expected) in [
        (
            "FREQ=MONTHLY;COUNT=4",
            vec!["2026-01-31", "2026-03-31", "2026-05-31", "2026-07-31"],
        ),
        (
            "FREQ=MONTHLY;BYMONTHDAY=31;COUNT=4",
            vec!["2026-01-31", "2026-03-31", "2026-05-31", "2026-07-31"],
        ),
        (
            "FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=4",
            vec!["2026-01-31", "2026-02-28", "2026-03-31", "2026-04-30"],
        ),
    ] {
        let text = event(&format!("DTSTART:20260131T100000Z\r\nRRULE:{rule}"));
        assert_eq!(days(&gather(&text, "2026-01-01T00:00:00Z", 8)), expected);
    }
}
#[test]
fn monthly_ordinal_weekdays_and_set_positions() {
    for (start, rule, expected) in [
        (
            "20260102",
            "BYDAY=MO,TU,WE,TH,FR;BYSETPOS=2",
            vec!["2026-01-02", "2026-02-03", "2026-03-03"],
        ),
        (
            "20260113",
            "BYDAY=2TU",
            vec!["2026-01-13", "2026-02-10", "2026-03-10"],
        ),
        (
            "20260130",
            "BYDAY=-1FR",
            vec!["2026-01-30", "2026-02-27", "2026-03-27"],
        ),
        (
            "20260130",
            "BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            vec!["2026-01-30", "2026-02-27", "2026-03-31"],
        ),
    ] {
        let text = event(&format!(
            "DTSTART:{start}T100000Z\r\nRRULE:FREQ=MONTHLY;{rule};COUNT=3"
        ));
        assert_eq!(days(&gather(&text, "2026-01-01T00:00:00Z", 4)), expected);
    }
}
#[test]
fn monthly_and_yearly_interval_count_and_inclusive_until() {
    for rule in [
        "FREQ=MONTHLY;INTERVAL=2;COUNT=3",
        "FREQ=MONTHLY;INTERVAL=2;UNTIL=20260513T100000Z",
    ] {
        let text = event(&format!("DTSTART:20260113T100000Z\r\nRRULE:{rule}"));
        assert_eq!(
            days(&gather(&text, "2026-01-01T00:00:00Z", 8)),
            ["2026-01-13", "2026-03-13", "2026-05-13"]
        );
    }
    for rule in [
        "FREQ=YEARLY;INTERVAL=2;COUNT=3",
        "FREQ=YEARLY;INTERVAL=2;UNTIL=20300113T100000Z",
    ] {
        let text = event(&format!("DTSTART:20260113T100000Z\r\nRRULE:{rule}"));
        assert_eq!(
            days(&gather(&text, "2026-01-01T00:00:00Z", 61)),
            ["2026-01-13", "2028-01-13", "2030-01-13"]
        );
    }
    let text = event(
        "DTSTART:20260310T100000Z\r\nRRULE:FREQ=YEARLY;BYMONTH=3,9;BYMONTHDAY=10;INTERVAL=2;COUNT=4",
    );
    assert_eq!(
        days(&gather(&text, "2026-01-01T00:00:00Z", 36)),
        ["2026-03-10", "2026-09-10", "2028-03-10", "2028-09-10"]
    );
}
#[test]
fn leap_day_is_skipped_without_consuming_count() {
    for rule in [
        "FREQ=YEARLY;COUNT=3",
        "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29;COUNT=3",
    ] {
        let text = event(&format!("DTSTART:20240229T100000Z\r\nRRULE:{rule}"));
        assert_eq!(
            days(&gather(&text, "2024-01-01T00:00:00Z", 108)),
            ["2024-02-29", "2028-02-29", "2032-02-29"]
        );
    }
}
#[test]
fn monthly_exdates_detached_overrides_and_rdates_share_the_same_exclusions() {
    let text=event("DTSTART:20260113T100000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=3\r\nEXDATE:20260113T100000Z\r\nRDATE:20260220T100000Z,20260221T100000Z\r\nEXDATE:20260221T100000Z")
        .replace("END:VCALENDAR","BEGIN:VEVENT\r\nUID:neutral-series\r\nRECURRENCE-ID:20260210T100000Z\r\nDTSTART:20260211T120000Z\r\nSUMMARY:Перенесённый обзор\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:neutral-series\r\nRECURRENCE-ID:20260310T100000Z\r\nSTATUS:CANCELLED\r\nDURATION:PT1H\r\nEND:VEVENT\r\nEND:VCALENDAR");
    let events = gather(&text, "2026-01-01T00:00:00Z", 4);
    assert_eq!(days(&events), ["2026-02-11", "2026-02-20"]);
    assert_eq!(events[0].link.title, "Перенесённый обзор");
}
#[test]
fn monthly_wall_clock_survives_berlin_dst_and_moscow_keeps_utc_offset() {
    for (zone, hours) in [
        ("Europe/Berlin", vec!["09:00", "09:00", "08:00"]),
        ("Europe/Moscow", vec!["07:00", "07:00", "07:00"]),
    ] {
        let text = event(&format!(
            "DTSTART;TZID={zone}:20260215T100000\r\nDTEND;TZID={zone}:20260215T110000\r\nRRULE:FREQ=MONTHLY;COUNT=3"
        ));
        let events = gather(&text, "2026-02-01T00:00:00Z", 4);
        assert_eq!(
            events
                .iter()
                .map(|e| e.start.format("%H:%M").to_string())
                .collect::<Vec<_>>(),
            hours
        );
        assert!(events.iter().all(|e| e.end - e.start == Duration::hours(1)));
    }
    let text = event(
        "DTSTART;TZID=W. Europe Standard Time:20260125T023000\r\nRRULE:FREQ=MONTHLY;BYDAY=-1SU;COUNT=3",
    );
    assert_eq!(
        days(&gather(&text, "2026-01-01T00:00:00Z", 5)),
        ["2026-01-25", "2026-02-22", "2026-04-26"]
    );
}

#[test]
fn unknown_feed_timezone_only_skips_events_that_need_it() {
    let text = "BEGIN:VCALENDAR\nX-WR-TIMEZONE:Unknown Standard Time\nBEGIN:VEVENT\nUID:utc\nSUMMARY:Встреча UTC\nDTSTART:20261115T100000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:explicit\nSUMMARY:Встреча в Берлине\nDTSTART;TZID=Europe/Berlin:20261116T100000\nDTEND:20261116T110000\nRDATE:20261117T100000\nEND:VEVENT\nBEGIN:VEVENT\nUID:floating\nSUMMARY:Встреча без зоны\nDTSTART:20261118T100000\nEND:VEVENT\nEND:VCALENDAR";
    let report = parse_ics_report(
        text,
        utc("2026-11-14T00:00:00Z"),
        utc("2026-11-19T00:00:00Z"),
        chrono_tz::UTC,
    )
    .unwrap();
    assert_eq!(report.events.len(), 3);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].title, "Встреча без зоны");
    assert_eq!(report.skipped[0].reason, SkipReason::Timezone);
    assert_eq!(report.events[1].start, utc("2026-11-16T09:00:00Z"));
    assert_eq!(report.events[2].start, utc("2026-11-17T09:00:00Z"));
}

#[test]
fn rdate_utc_keeps_the_second_berlin_fold_instant() {
    let text = event(
        "DTSTART;TZID=Europe/Berlin:20261024T023000\r\nDTEND;TZID=Europe/Berlin:20261024T033000\r\nRDATE:20261025T003000Z,20261025T013000Z\r\nEXDATE:20261025T003000Z",
    );
    let report = parse_ics_report(
        &text,
        utc("2026-10-24T00:00:00Z"),
        utc("2026-10-26T00:00:00Z"),
        chrono_tz::UTC,
    )
    .unwrap();
    assert!(report.skipped.is_empty());
    assert_eq!(report.events.len(), 2);
    assert_eq!(report.events[1].start, utc("2026-10-25T01:30:00Z"));
}

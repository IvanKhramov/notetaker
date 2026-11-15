// Preserved day walker from 3575c6a, used only as a test oracle.
use super::*;
fn walk(
    value: &str,
    start: &Date,
    to: DateTime<Utc>,
    budget: &mut usize,
    mut add: impl FnMut(NaiveDateTime) -> Result<()>,
) -> Result<()> {
    let rule = Rule::parse(value, start)?;
    let first_weekday = start.wall.weekday().num_days_from_monday() as usize;
    let mut emitted = 0;
    let mut period = None;
    let mut candidates = Vec::new();
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
        if begin > to || rule.until.is_some_and(|u| begin > u) {
            return Ok(());
        }
        let months = (wall.year() - start.wall.year()) as usize * 12 + wall.month() as usize
            - start.wall.month() as usize;
        let selected = match rule.frequency {
            "DAILY" => {
                day.is_multiple_of(rule.interval)
                    && rule.day_matches(wall.date(), start.wall.date())
            }
            "WEEKLY" => {
                ((day + (first_weekday + 7 - rule.week_start) % 7) / 7)
                    .is_multiple_of(rule.interval)
                    && rule.day_matches(wall.date(), start.wall.date())
                    && (!rule.weekdays.is_empty()
                        || wall.weekday().num_days_from_monday() as usize == first_weekday)
            }
            _ => {
                let key = (
                    wall.year(),
                    if rule.frequency == "YEARLY" {
                        0
                    } else {
                        wall.month()
                    },
                );
                if period != Some(key) {
                    candidates = rule.period_dates(wall.date(), start.wall.date())?;
                    period = Some(key);
                }
                (if rule.frequency == "MONTHLY" {
                    months
                } else {
                    (wall.year() - start.wall.year()) as usize
                })
                .is_multiple_of(rule.interval)
                    && candidates.contains(&wall.date())
            }
        };
        if selected || day == 0 {
            emitted += 1;
            add(wall)?;
            if emitted >= rule.count {
                return Ok(());
            }
        }
    }
    Err(CalendarError("повтор длиннее 100000 дней"))
}

#[test]
fn period_expansion_matches_the_old_walker_on_seeded_rules() {
    let start = Date {
        wall: NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap(),
        zone: chrono_tz::UTC,
        all_day: false,
    };
    let mut seed = 0x1234_5678u64;
    let forms = [
        "FREQ=DAILY",
        "FREQ=DAILY;BYDAY=MO,WE,FR",
        "FREQ=WEEKLY;BYDAY=TU,TH",
        "FREQ=WEEKLY;BYDAY=MO,FR;WKST=SU",
        "FREQ=MONTHLY;BYMONTHDAY=31",
        "FREQ=MONTHLY;BYDAY=2TU",
        "FREQ=MONTHLY;BYDAY=5FR",
        "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
        "FREQ=YEARLY",
        "FREQ=YEARLY;BYMONTH=2,7;BYMONTHDAY=-1",
    ];
    for i in 0..120 {
        let mut start = start.clone();
        start.zone = [
            chrono_tz::UTC,
            chrono_tz::Europe::Berlin,
            chrono_tz::Europe::Moscow,
        ][i % 3];
        if i % 4 == 0 {
            start.wall = start.wall.date().and_hms_opt(2, 30, 0).unwrap();
        }
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let rule = format!(
            "{};INTERVAL={}{}",
            forms[i % forms.len()],
            1 + seed % 3,
            if i % 3 == 0 {
                format!(";COUNT={}", 1 + seed % 100)
            } else if i % 3 == 1 {
                ";UNTIL=20231231T235959Z".into()
            } else {
                String::new()
            }
        );
        let from = wall_utc(
            start.zone,
            (start.wall + Duration::days((seed % 1800) as i64))
                .date()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
        )
        .unwrap();
        let to = from + Duration::days(20);
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        walk(&rule, &start, to, &mut 1_000_000, |wall| {
            if wall_utc(start.zone, wall)? >= from {
                expected.push(wall);
            }
            Ok(())
        })
        .unwrap();
        expand(&rule, &start, from, to, &mut 100_000, |wall| {
            if wall_utc(start.zone, wall)? >= from {
                actual.push(wall);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(actual, expected, "{rule}, {from}");
    }
}

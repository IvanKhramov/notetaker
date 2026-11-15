use super::*;

struct Rule<'a> {
    frequency: &'a str,
    interval: usize,
    count: usize,
    until: Option<DateTime<Utc>>,
    weekdays: Vec<(Option<i32>, u32)>,
    monthdays: Vec<i32>,
    months: Vec<i32>,
    positions: Vec<i32>,
    week_start: usize,
}
fn integers(value: Option<&&str>, limit: i32, negative: bool) -> Result<Vec<i32>> {
    value.map_or(Ok(Vec::new()), |v| {
        v.split(',')
            .map(|v| {
                v.parse::<i32>()
                    .ok()
                    .filter(|n| *n != 0 && *n <= limit && *n >= if negative { -limit } else { 1 })
                    .ok_or(CalendarError("число RRULE"))
            })
            .collect()
    })
}
impl<'a> Rule<'a> {
    fn parse(value: &'a str, start: &Date) -> Result<Self> {
        let mut fields = BTreeMap::new();
        for part in value.split(';') {
            let (key, value) = part.split_once('=').ok_or(CalendarError("RRULE"))?;
            if ![
                "FREQ",
                "COUNT",
                "UNTIL",
                "BYDAY",
                "INTERVAL",
                "WKST",
                "BYMONTHDAY",
                "BYMONTH",
                "BYSETPOS",
            ]
            .contains(&key)
                || fields.insert(key, value).is_some()
            {
                return Err(CalendarError("параметр RRULE"));
            }
        }
        let frequency = *fields.get("FREQ").ok_or(CalendarError("нет FREQ"))?;
        if !["DAILY", "WEEKLY", "MONTHLY", "YEARLY"].contains(&frequency) {
            return Err(CalendarError("неподдерживаемый FREQ"));
        }
        let number = |key, default| -> Result<usize> {
            fields.get(key).map_or(Ok(default), |v| {
                v.parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(CalendarError("число RRULE"))
            })
        };
        let days = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
        let mut weekdays = Vec::new();
        if let Some(value) = fields.get("BYDAY") {
            for v in value.split(',') {
                let at = v.len().checked_sub(2).ok_or(CalendarError("BYDAY"))?;
                let (ordinal, day) = v.split_at_checked(at).ok_or(CalendarError("BYDAY"))?;
                let weekday = days
                    .iter()
                    .position(|d| *d == day)
                    .ok_or(CalendarError("BYDAY"))? as u32;
                let ordinal = if ordinal.is_empty() {
                    None
                } else {
                    Some(
                        ordinal
                            .parse::<i32>()
                            .ok()
                            .filter(|n| *n != 0 && (-53..=53).contains(n))
                            .ok_or(CalendarError("BYDAY"))?,
                    )
                };
                if ordinal.is_some() && !["MONTHLY", "YEARLY"].contains(&frequency) {
                    return Err(CalendarError("порядковый BYDAY"));
                }
                weekdays.push((ordinal, weekday));
            }
        }
        weekdays.sort_unstable();
        weekdays.dedup();
        let mut monthdays = integers(fields.get("BYMONTHDAY"), 31, true)?;
        monthdays.sort_unstable();
        monthdays.dedup();
        let mut months = integers(fields.get("BYMONTH"), 12, false)?;
        months.sort_unstable();
        months.dedup();
        let mut positions = integers(fields.get("BYSETPOS"), 366, true)?;
        positions.sort_unstable();
        positions.dedup();
        if frequency == "WEEKLY" && !monthdays.is_empty() {
            return Err(CalendarError("BYMONTHDAY при WEEKLY"));
        }
        if !positions.is_empty()
            && (!["MONTHLY", "YEARLY"].contains(&frequency)
                || (weekdays.is_empty() && monthdays.is_empty() && months.is_empty()))
        {
            return Err(CalendarError("неподдерживаемый BYSETPOS"));
        }
        Ok(Self {
            frequency,
            interval: number("INTERVAL", 1)?,
            count: number("COUNT", usize::MAX)?,
            until: fields
                .get("UNTIL")
                .map(|v| date(v, &BTreeMap::new(), start.zone).and_then(|d| d.utc()))
                .transpose()?,
            week_start: fields.get("WKST").map_or(Ok(0), |v| {
                days.iter()
                    .position(|d| d == v)
                    .ok_or(CalendarError("WKST"))
            })?,
            weekdays,
            monthdays,
            months,
            positions,
        })
    }
    fn day_matches(&self, day: NaiveDate, start: NaiveDate) -> bool {
        if !self.months.is_empty() && !self.months.contains(&(day.month() as i32)) {
            return false;
        }
        let month_length = month_length(day.year(), day.month());
        if !self.monthdays.is_empty()
            && !self
                .monthdays
                .iter()
                .any(|n| day.day() as i32 == if *n > 0 { *n } else { month_length + 1 + n })
        {
            return false;
        }
        if !self.weekdays.is_empty() {
            if !self.weekdays.iter().any(|(ordinal, weekday)| {
                if day.weekday().num_days_from_monday() != *weekday {
                    return false;
                }
                ordinal.is_none_or(|n| {
                    let (position, length) = if self.frequency == "YEARLY" && self.months.is_empty()
                    {
                        (
                            day.ordinal() as i32,
                            if NaiveDate::from_ymd_opt(day.year(), 2, 29).is_some() {
                                366
                            } else {
                                365
                            },
                        )
                    } else {
                        (day.day() as i32, month_length)
                    };
                    n == if n > 0 {
                        (position - 1) / 7 + 1
                    } else {
                        -((length - position) / 7 + 1)
                    }
                })
            }) {
                return false;
            }
        } else if self.monthdays.is_empty()
            && ["MONTHLY", "YEARLY"].contains(&self.frequency)
            && day.day() != start.day()
        {
            return false;
        }
        !(self.frequency == "YEARLY"
            && self.months.is_empty()
            && self.weekdays.is_empty()
            && self.monthdays.is_empty()
            && day.month() != start.month())
    }
    fn period_dates(&self, day: NaiveDate, start: NaiveDate) -> Result<Vec<NaiveDate>> {
        let first = NaiveDate::from_ymd_opt(
            day.year(),
            if self.frequency == "YEARLY" {
                1
            } else {
                day.month()
            },
            1,
        )
        .ok_or(CalendarError("дата"))?;
        let mut candidates = Vec::new();
        for i in 0..366 {
            let current = first
                .checked_add_signed(Duration::days(i))
                .ok_or(CalendarError("дата"))?;
            if current.year() != day.year()
                || (self.frequency == "MONTHLY" && current.month() != day.month())
            {
                break;
            }
            if self.day_matches(current, start) {
                candidates.push(current);
            }
        }
        if self.positions.is_empty() {
            return Ok(candidates);
        }
        let mut selected = Vec::new();
        for position in &self.positions {
            let index = if *position > 0 {
                *position - 1
            } else {
                candidates.len() as i32 + position
            };
            if let Ok(index) = usize::try_from(index)
                && let Some(date) = candidates.get(index)
            {
                selected.push(*date);
            }
        }
        Ok(selected)
    }
}
fn month_length(year: i32, month: u32) -> i32 {
    (28..=31)
        .rev()
        .find(|day| NaiveDate::from_ymd_opt(year, month, *day).is_some())
        .unwrap_or(28) as i32
}
impl Rule<'_> {
    // Index of a selected period, rather than a calendar day since DTSTART.
    fn index_at(&self, date: NaiveDate, start: &Date) -> usize {
        let days = (date - start.wall.date()).num_days().max(0) as usize;
        let raw = match self.frequency {
            "DAILY" => days,
            "WEEKLY" => {
                (days
                    + (start.wall.weekday().num_days_from_monday() as usize + 7 - self.week_start)
                        % 7)
                    / 7
            }
            "MONTHLY" => ((date.year() - start.wall.year()) as i64 * 12 + i64::from(date.month())
                - i64::from(start.wall.month()))
            .max(0) as usize,
            _ => (date.year() - start.wall.year()).max(0) as usize,
        };
        raw / self.interval
    }
    fn period_start(&self, index: usize, start: &Date) -> Result<NaiveDate> {
        let distance = index
            .checked_mul(self.interval)
            .ok_or(CalendarError("дата вне диапазона"))?;
        let date = start.wall.date();
        let invalid = || CalendarError("дата вне диапазона");
        match self.frequency {
            "DAILY" => date
                .checked_add_days(chrono::Days::new(
                    u64::try_from(distance).map_err(|_| invalid())?,
                ))
                .ok_or_else(invalid),
            "WEEKLY" => {
                let offset =
                    (date.weekday().num_days_from_monday() as usize + 7 - self.week_start) % 7;
                date.checked_sub_signed(Duration::days(offset as i64))
                    .and_then(|d| {
                        d.checked_add_days(chrono::Days::new(
                            u64::try_from(distance).ok()?.checked_mul(7)?,
                        ))
                    })
                    .ok_or_else(invalid)
            }
            "MONTHLY" => date
                .with_day(1)
                .and_then(|d| {
                    d.checked_add_months(chrono::Months::new(u32::try_from(distance).ok()?))
                })
                .ok_or_else(invalid),
            _ => NaiveDate::from_ymd_opt(
                date.year()
                    .checked_add(i32::try_from(distance).map_err(|_| invalid())?)
                    .ok_or_else(invalid)?,
                1,
                1,
            )
            .ok_or_else(invalid),
        }
    }
    fn candidates(
        &self,
        index: usize,
        start: &Date,
        budget: &mut usize,
    ) -> Result<Vec<NaiveDateTime>> {
        // Charge predicate work as well as period steps, so large BYxxx sets
        // cannot multiply the per-event budget into an unbounded date scan.
        let days = match self.frequency {
            "DAILY" => 1,
            "WEEKLY" => 7,
            "MONTHLY" => 31,
            _ => 366,
        };
        let cost = days * (1 + self.weekdays.len() + self.monthdays.len() + self.months.len())
            + self.positions.len();
        *budget = budget
            .checked_sub(cost)
            .ok_or(CalendarError("превышен лимит расчёта повторов"))?;
        let anchor = self.period_start(index, start)?;
        let dates = match self.frequency {
            "DAILY" => {
                if self.day_matches(anchor, start.wall.date()) {
                    vec![anchor]
                } else {
                    vec![]
                }
            }
            "WEEKLY" => {
                let mut dates = Vec::new();
                for day in 0..7 {
                    let date = anchor
                        .checked_add_signed(Duration::days(day))
                        .ok_or(CalendarError("дата вне диапазона"))?;
                    if self.day_matches(date, start.wall.date())
                        && (!self.weekdays.is_empty() || date.weekday() == start.wall.weekday())
                    {
                        dates.push(date);
                    }
                }
                dates
            }
            _ => self.period_dates(anchor, start.wall.date())?,
        };
        let mut dates: Vec<_> = dates
            .into_iter()
            .map(|date| date.and_time(start.wall.time()))
            .filter(|wall| *wall >= start.wall && wall_utc(start.zone, *wall).is_ok())
            .collect();
        if index == 0 {
            dates.push(start.wall);
        }
        dates.sort_unstable();
        dates.dedup();
        Ok(dates)
    }
    fn count_before(&self, index: usize, start: &Date, budget: &mut usize) -> Result<usize> {
        if index == 0 {
            return Ok(0);
        }
        // UTC has no invalid spring wall times. Month filters need per-period counting.
        if matches!(
            start.zone,
            chrono_tz::UTC | chrono_tz::Etc::UTC | chrono_tz::Etc::GMT
        ) && self.months.is_empty()
            && self.monthdays.is_empty()
        {
            if self.frequency == "DAILY" {
                let weekday = start.wall.weekday().num_days_from_monday();
                let allowed = |p: usize| {
                    self.weekdays.is_empty()
                        || self.weekdays.iter().any(|(_, day)| {
                            *day == (weekday + (p % 7 * self.interval.rem_euclid(7)) as u32) % 7
                        })
                };
                let cycle = (0..7).filter(|p| allowed(*p)).count();
                return Ok((index / 7).saturating_mul(cycle)
                    + (0..index % 7).filter(|p| allowed(*p)).count()
                    + usize::from(!allowed(0)));
            }
            if self.frequency == "WEEKLY" {
                let mut days: Vec<_> = self.weekdays.iter().map(|(_, day)| *day).collect();
                days.sort_unstable();
                days.dedup();
                let per_week = days.len().max(1);
                return Ok(
                    self.candidates(0, start, budget)?.len() + (index - 1).saturating_mul(per_week)
                );
            }
            let ordinal_limit = match self.frequency {
                "MONTHLY" => 4,
                "YEARLY" => 52,
                _ => 0,
            };
            let ordinal_one = self.positions.is_empty()
                && self.weekdays.len() == 1
                && self.weekdays[0]
                    .0
                    .is_some_and(|n| n.unsigned_abs() <= ordinal_limit);
            let set_one = self.frequency == "MONTHLY"
                && self.positions.len() == 1
                && !self.weekdays.is_empty()
                && self.weekdays.iter().all(|(ordinal, _)| ordinal.is_none())
                && self.positions[0].unsigned_abs() as usize <= 4 * self.weekdays.len();
            if ordinal_one || set_one {
                // Every later selected period has exactly one date. DTSTART can
                // add an extra first-period occurrence when it is off-pattern.
                return Ok(self.candidates(0, start, budget)?.len() + index - 1);
            }
            if self.weekdays.is_empty()
                && self.positions.is_empty()
                && ((self.frequency == "MONTHLY" && start.wall.day() <= 28)
                    || (self.frequency == "YEARLY"
                        && !(start.wall.month() == 2 && start.wall.day() == 29)))
            {
                return Ok(index);
            }
        }
        let mut count = 0usize;
        for p in 0..index {
            charge(budget)?;
            for wall in self.candidates(p, start, budget)? {
                let begin = wall_utc(start.zone, wall)?;
                if self.until.is_some_and(|until| begin > until) {
                    return Ok(self.count);
                }
                count = count.saturating_add(1);
                if count >= self.count {
                    return Ok(count);
                }
            }
        }
        Ok(count)
    }
}
fn charge(budget: &mut usize) -> Result<()> {
    *budget = budget
        .checked_sub(1)
        .ok_or(CalendarError("превышен лимит расчёта повторов"))?;
    Ok(())
}
pub(super) fn expand(
    value: &str,
    start: &Date,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    budget: &mut usize,
    mut add: impl FnMut(NaiveDateTime) -> Result<()>,
) -> Result<()> {
    let rule = Rule::parse(value, start)?;
    let mut index = rule.index_at(from.with_timezone(&start.zone).date_naive(), start);
    let mut emitted = if rule.count == usize::MAX {
        0
    } else {
        rule.count_before(index, start, budget)?
    };
    if emitted >= rule.count {
        return Ok(());
    }
    let last = to.with_timezone(&start.zone).date_naive();
    loop {
        charge(budget)?;
        // A period beyond the calendar's date range is beyond the window too:
        // a huge INTERVAL must not discard the occurrences already found.
        match rule.period_start(index, start) {
            Ok(period) if period <= last => {}
            _ => return Ok(()),
        }
        for wall in rule.candidates(index, start, budget)? {
            let begin = wall_utc(start.zone, wall)?;
            if begin > to || rule.until.is_some_and(|until| begin > until) {
                return Ok(());
            }
            emitted = emitted.saturating_add(1);
            add(wall)?;
            if emitted >= rule.count {
                return Ok(());
            }
        }
        index = index
            .checked_add(1)
            .ok_or(CalendarError("дата вне диапазона"))?;
    }
}

#[cfg(test)]
#[path = "walker_oracle.rs"]
mod walker_oracle;

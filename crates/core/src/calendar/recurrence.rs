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
        let monthdays = integers(fields.get("BYMONTHDAY"), 31, true)?;
        let months = integers(fields.get("BYMONTH"), 12, false)?;
        let positions = integers(fields.get("BYSETPOS"), 366, true)?;
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
pub(super) fn expand(
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

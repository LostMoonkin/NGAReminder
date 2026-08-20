use std::cmp::{max, min};

use time::{Duration, OffsetDateTime, Time, UtcOffset, Weekday};

pub const MAX_NO_FETCH_PERIODS: usize = 128;
const MINUTES_PER_DAY: i32 = 1_440;
const MINUTES_PER_WEEK: usize = 7 * MINUTES_PER_DAY as usize;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NoFetchPeriod {
    pub days: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub end_time: String,
    pub start_time: String,
}

pub type NoFetchPeriods = Vec<NoFetchPeriod>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoFetchWindow {
    pub start: OffsetDateTime,
    pub until: OffsetDateTime,
}

/// Validate a complete no-fetch configuration.
///
/// Empty configurations are represented as `None` by the persistence layer;
/// accepting an empty array here would make PATCH's replacement semantics
/// ambiguous, so it is deliberately rejected.
pub fn validate_no_fetch_periods(periods: &[NoFetchPeriod]) -> Result<(), String> {
    if periods.is_empty() {
        return Err("no_fetch_periods must contain at least one rule".to_owned());
    }
    if periods.len() > MAX_NO_FETCH_PERIODS {
        return Err(format!(
            "no_fetch_periods must contain at most {MAX_NO_FETCH_PERIODS} rules"
        ));
    }

    for (index, period) in periods.iter().enumerate() {
        if period.days.is_empty() {
            return Err(format!(
                "no-fetch period {index} must contain at least one day"
            ));
        }
        let start = parse_time(&period.start_time, false)
            .map_err(|error| format!("no-fetch period {index} start_time: {error}"))?;
        let end = parse_time(&period.end_time, true)
            .map_err(|error| format!("no-fetch period {index} end_time: {error}"))?;
        if start == end {
            return Err(format!(
                "no-fetch period {index} start_time and end_time must differ"
            ));
        }
        for day in &period.days {
            if !all_weekdays()
                .iter()
                .any(|weekday| matches_day(day, *weekday))
            {
                return Err(format!(
                    "no-fetch period {index} has an unsupported day: {day}"
                ));
            }
        }
    }

    let mut covered = [false; MINUTES_PER_WEEK];
    for period in periods {
        let start = parse_time(&period.start_time, false).expect("validated start time");
        let end = parse_time(&period.end_time, true).expect("validated end time");
        let duration = if end > start {
            end - start
        } else {
            end + MINUTES_PER_DAY - start
        } as usize;
        for day in &period.days {
            for weekday in all_weekdays()
                .into_iter()
                .filter(|weekday| matches_day(day, *weekday))
            {
                let day_index = weekday_index(weekday) * MINUTES_PER_DAY as usize;
                for minute in 0..duration {
                    covered[(day_index + start as usize + minute) % MINUTES_PER_WEEK] = true;
                }
            }
        }
    }
    if covered.iter().all(|minute| *minute) {
        return Err(
            "no_fetch_periods must leave at least one minute available each week".to_owned(),
        );
    }
    Ok(())
}

/// Return the contiguous no-fetch window containing `now`, if any.
///
/// Rules are expanded over a small weekly horizon and merged as intervals.
/// This makes overlap and adjacency independent of rule ordering and also
/// handles cross-midnight rules whose owning weekday is the start weekday.
pub fn current_window(
    periods: Option<&NoFetchPeriods>,
    now: OffsetDateTime,
    timezone_offset: UtcOffset,
) -> Option<NoFetchWindow> {
    let periods = periods.filter(|items| !items.is_empty())?;
    let local_now = now.to_offset(timezone_offset);
    let mut intervals = Vec::with_capacity(periods.len() * 4);

    for day_offset in -8_i64..=16 {
        let date = local_now.date().saturating_add(Duration::days(day_offset));
        for period in periods {
            if !period
                .days
                .iter()
                .any(|day| matches_day(day, date.weekday()))
            {
                continue;
            }
            let start = parse_time(&period.start_time, false).ok()?;
            let end = parse_time(&period.end_time, true).ok()?;
            let start_time = minutes_to_time(start)?;
            let start_at = OffsetDateTime::new_in_offset(date, start_time, timezone_offset);
            let end_date = if end <= start {
                date.saturating_add(Duration::days(1))
            } else {
                date
            };
            let end_at = if end == MINUTES_PER_DAY {
                OffsetDateTime::new_in_offset(
                    date.saturating_add(Duration::days(1)),
                    Time::MIDNIGHT,
                    timezone_offset,
                )
            } else {
                OffsetDateTime::new_in_offset(end_date, minutes_to_time(end)?, timezone_offset)
            };
            if end_at > start_at {
                intervals.push((start_at, end_at));
            }
        }
    }

    intervals.sort_unstable_by_key(|left| left.0);
    let mut window_start = None;
    let mut window_end = None;
    for (start, end) in &intervals {
        if *start <= local_now && local_now < *end {
            window_start = Some(window_start.map_or(*start, |value| min(value, *start)));
            window_end = Some(window_end.map_or(*end, |value| max(value, *end)));
        }
    }
    let (Some(mut start), Some(mut until)) = (window_start, window_end) else {
        return None;
    };

    loop {
        let previous_start = start;
        let previous_end = until;
        for (interval_start, interval_end) in &intervals {
            if *interval_start <= until && *interval_end >= start {
                start = min(start, *interval_start);
                until = max(until, *interval_end);
            }
        }
        if start == previous_start && until == previous_end {
            break;
        }
    }
    Some(NoFetchWindow { start, until })
}

pub fn format_rfc3339(value: OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting must succeed")
}

/// Format an instant for PostgreSQL's `TIMESTAMPTZ` parser without relying on
/// the database session's timezone.
pub fn format_postgres_timestamp(value: OffsetDateTime) -> String {
    format_rfc3339(value.to_offset(UtcOffset::UTC))
}

/// Format a UTC timestamp in SQLite's representation for scheduler columns.
pub fn format_database_timestamp(value: OffsetDateTime) -> String {
    let value = value.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        value.year(),
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second()
    )
}

fn matches_day(value: &str, weekday: Weekday) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "weekdays" => !matches!(weekday, Weekday::Saturday | Weekday::Sunday),
        "weekends" => matches!(weekday, Weekday::Saturday | Weekday::Sunday),
        "monday" | "mon" => weekday == Weekday::Monday,
        "tuesday" | "tue" | "tues" => weekday == Weekday::Tuesday,
        "wednesday" | "wed" => weekday == Weekday::Wednesday,
        "thursday" | "thu" | "thur" | "thurs" => weekday == Weekday::Thursday,
        "friday" | "fri" => weekday == Weekday::Friday,
        "saturday" | "sat" => weekday == Weekday::Saturday,
        "sunday" | "sun" => weekday == Weekday::Sunday,
        _ => false,
    }
}

fn all_weekdays() -> [Weekday; 7] {
    [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ]
}

fn weekday_index(weekday: Weekday) -> usize {
    match weekday {
        Weekday::Monday => 0,
        Weekday::Tuesday => 1,
        Weekday::Wednesday => 2,
        Weekday::Thursday => 3,
        Weekday::Friday => 4,
        Weekday::Saturday => 5,
        Weekday::Sunday => 6,
    }
}

fn parse_time(value: &str, allow_end_of_day: bool) -> Result<i32, String> {
    let mut parts = value.split(':');
    let hour = parts
        .next()
        .ok_or_else(|| "must use HH:MM format".to_owned())?
        .parse::<i32>()
        .map_err(|_| "must use HH:MM format".to_owned())?;
    let minute = parts
        .next()
        .ok_or_else(|| "must use HH:MM format".to_owned())?
        .parse::<i32>()
        .map_err(|_| "must use HH:MM format".to_owned())?;
    if parts.next().is_some()
        || !(0..60).contains(&minute)
        || hour < 0
        || (hour > 23 && !(allow_end_of_day && hour == 24 && minute == 0))
    {
        return Err("must use HH:MM format".to_owned());
    }
    Ok(hour * 60 + minute)
}

fn minutes_to_time(minutes: i32) -> Option<Time> {
    if !(0..MINUTES_PER_DAY).contains(&minutes) {
        return None;
    }
    Time::from_hms((minutes / 60) as u8, (minutes % 60) as u8, 0).ok()
}

#[cfg(test)]
mod tests {
    use time::{Date, Month, OffsetDateTime, UtcOffset};

    use super::{NoFetchPeriod, current_window, validate_no_fetch_periods};

    fn period(days: &[&str], start: &str, end: &str) -> NoFetchPeriod {
        NoFetchPeriod {
            days: days.iter().map(|day| (*day).to_owned()).collect(),
            description: None,
            end_time: end.to_owned(),
            start_time: start.to_owned(),
        }
    }

    fn at(day: u8, hour: u8, minute: u8) -> OffsetDateTime {
        OffsetDateTime::new_in_offset(
            Date::from_calendar_date(2026, Month::August, day).expect("test date must be valid"),
            time::Time::from_hms(hour, minute, 0).expect("test time must be valid"),
            UtcOffset::UTC,
        )
    }

    #[test]
    fn validates_boundaries_and_rejects_empty_or_equal_periods() {
        assert!(validate_no_fetch_periods(&[]).is_err());
        assert!(validate_no_fetch_periods(&[period(&["monday"], "08:00", "08:00")]).is_err());
        validate_no_fetch_periods(&[period(&["monday"], "00:00", "24:00")])
            .expect("an ordinary all-day rule must validate");
    }

    #[test]
    fn rejects_a_configuration_that_covers_the_full_week() {
        let periods = vec![
            period(&["weekdays"], "00:00", "24:00"),
            period(&["weekends"], "00:00", "24:00"),
        ];
        assert!(validate_no_fetch_periods(&periods).is_err());
    }

    #[test]
    fn uses_inclusive_start_and_exclusive_end() {
        let periods = vec![period(&["monday"], "08:00", "09:00")];
        let window = current_window(Some(&periods), at(17, 8, 0), UtcOffset::UTC)
            .expect("start must be active");
        assert_eq!(window.until, at(17, 9, 0));
        assert!(current_window(Some(&periods), at(17, 9, 0), UtcOffset::UTC).is_none());
    }

    #[test]
    fn cross_midnight_belongs_to_the_start_day() {
        let periods = vec![period(&["monday"], "23:00", "02:00")];
        let window = current_window(Some(&periods), at(18, 1, 30), UtcOffset::UTC)
            .expect("the following morning must be active");
        assert_eq!(window.start, at(17, 23, 0));
        assert_eq!(window.until, at(18, 2, 0));
        assert!(current_window(Some(&periods), at(19, 1, 30), UtcOffset::UTC).is_none());
    }

    #[test]
    fn overlapping_and_adjacent_periods_merge() {
        let periods = vec![
            period(&["monday"], "08:00", "09:00"),
            period(&["monday"], "09:00", "10:00"),
            period(&["monday"], "08:30", "11:00"),
        ];
        let window = current_window(Some(&periods), at(17, 8, 45), UtcOffset::UTC)
            .expect("the merged period must be active");
        assert_eq!(window.start, at(17, 8, 0));
        assert_eq!(window.until, at(17, 11, 0));
    }

    #[test]
    fn evaluates_the_rule_in_the_configured_fixed_offset() {
        let periods = vec![period(&["monday"], "08:00", "09:00")];
        let offset = UtcOffset::from_hms(8, 0, 0).expect("offset must be valid");
        let now = at(17, 0, 30);
        let window = current_window(Some(&periods), now, offset)
            .expect("Monday 08:30 in the service offset must be active");
        assert_eq!(window.until, at(17, 1, 0));
    }
}

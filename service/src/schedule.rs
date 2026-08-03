use time::{Date, Duration, OffsetDateTime, Time, UtcOffset, Weekday};

pub const MIN_INTERVAL_SECONDS: i32 = 30;
pub const MAX_INTERVAL_SECONDS: i32 = 86_400;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduleRule {
    pub days: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub end_time: String,
    pub interval: i32,
    pub start_time: String,
}

pub type Schedule = Vec<ScheduleRule>;

pub fn validate_interval(value: i32) -> bool {
    (MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&value)
}

pub fn validate_schedule(schedule: &[ScheduleRule]) -> Result<(), String> {
    if schedule.len() > 128 {
        return Err("schedule must contain at most 128 rules".to_owned());
    }
    for (index, rule) in schedule.iter().enumerate() {
        if rule.days.is_empty() {
            return Err(format!(
                "schedule rule {index} must contain at least one day"
            ));
        }
        if !validate_interval(rule.interval) {
            return Err(format!("schedule rule {index} has an invalid interval"));
        }
        parse_time(&rule.start_time, false)
            .map_err(|error| format!("schedule rule {index} start_time: {error}"))?;
        parse_time(&rule.end_time, true)
            .map_err(|error| format!("schedule rule {index} end_time: {error}"))?;
        for day in &rule.days {
            if !is_supported_day(day) {
                return Err(format!(
                    "schedule rule {index} has an unsupported day: {day}"
                ));
            }
        }
    }
    Ok(())
}

pub fn next_delay_seconds(
    schedule: Option<&Schedule>,
    fallback_interval: i32,
    now: OffsetDateTime,
    timezone_offset: UtcOffset,
) -> i64 {
    let fallback = i64::from(fallback_interval.max(MIN_INTERVAL_SECONDS));
    let Some(schedule) = schedule.filter(|items| !items.is_empty()) else {
        return fallback;
    };

    let local_now = now.to_offset(timezone_offset);
    let active_interval = schedule.iter().find_map(|rule| {
        let start = parse_time(&rule.start_time, false).ok()?;
        let end = normalize_end(start, parse_time(&rule.end_time, true).ok()?);
        if rule_is_active(rule, local_now, start, end) {
            Some(i64::from(rule.interval))
        } else {
            None
        }
    });
    let interval = active_interval.unwrap_or(fallback);
    let next_boundary = next_boundary(schedule, local_now, timezone_offset);
    match next_boundary {
        Some(boundary) => interval.min(boundary.max(1)),
        None => interval,
    }
}

fn next_boundary(
    schedule: &[ScheduleRule],
    local_now: OffsetDateTime,
    timezone_offset: UtcOffset,
) -> Option<i64> {
    let mut nearest: Option<i64> = None;
    for day_offset in -1_i64..=8 {
        let date = local_now.date().saturating_add(Duration::days(day_offset));
        for rule in schedule {
            if !rule_matches_day(rule, date.weekday()) {
                continue;
            }
            let start = parse_time(&rule.start_time, false).ok()?;
            let end = normalize_end(start, parse_time(&rule.end_time, true).ok()?);
            for (boundary_date, boundary_time) in boundaries(date, start, end) {
                let boundary =
                    OffsetDateTime::new_in_offset(boundary_date, boundary_time, timezone_offset);
                if boundary <= local_now {
                    continue;
                }
                let seconds = (boundary - local_now).whole_seconds();
                nearest = Some(nearest.map_or(seconds, |current| current.min(seconds)));
            }
        }
    }
    nearest
}

fn boundaries(date: Date, start: i32, end: i32) -> [(Date, Time); 2] {
    let start_time = minutes_to_time(start).expect("validated schedule start time");
    if end == 1_440 {
        return [
            (date, start_time),
            (date.saturating_add(Duration::days(1)), Time::MIDNIGHT),
        ];
    }
    let end_date = if end <= start {
        date.saturating_add(Duration::days(1))
    } else {
        date
    };
    [
        (date, start_time),
        (
            end_date,
            minutes_to_time(end).expect("validated schedule end time"),
        ),
    ]
}

fn time_in_window(time: Time, start: i32, end: i32) -> bool {
    let current = time.hour() as i32 * 60 + time.minute() as i32;
    if end == 1_440 {
        current >= start
    } else if end == start {
        true
    } else if end > start {
        current >= start && current < end
    } else {
        current >= start || current < end
    }
}

fn rule_is_active(rule: &ScheduleRule, now: OffsetDateTime, start: i32, end: i32) -> bool {
    if rule_matches_day(rule, now.date().weekday()) && time_in_window(now.time(), start, end) {
        return true;
    }
    if end >= start {
        return false;
    }
    let previous_weekday = now.date().saturating_sub(Duration::days(1)).weekday();
    rule_matches_day(rule, previous_weekday)
        && (now.time().hour() as i32 * 60 + now.time().minute() as i32) < end
}

fn normalize_end(start: i32, end: i32) -> i32 {
    // Treat the common 00:00-23:59 form as a full-day window.
    if start == 0 && end == 1_439 {
        1_440
    } else {
        end
    }
}

fn rule_matches_day(rule: &ScheduleRule, weekday: Weekday) -> bool {
    rule.days
        .iter()
        .any(|day| match day.to_ascii_lowercase().as_str() {
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
        })
}

fn is_supported_day(day: &str) -> bool {
    matches!(
        day.to_ascii_lowercase().as_str(),
        "weekdays"
            | "weekends"
            | "monday"
            | "mon"
            | "tuesday"
            | "tue"
            | "tues"
            | "wednesday"
            | "wed"
            | "thursday"
            | "thu"
            | "thur"
            | "thurs"
            | "friday"
            | "fri"
            | "saturday"
            | "sat"
            | "sunday"
            | "sun"
    )
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
    if !(0..1_440).contains(&minutes) {
        return None;
    }
    Time::from_hms((minutes / 60) as u8, (minutes % 60) as u8, 0).ok()
}

#[cfg(test)]
mod tests {
    use time::{Date, Month, PrimitiveDateTime, UtcOffset};

    use super::{ScheduleRule, next_delay_seconds, validate_schedule};

    fn rule(days: &[&str], start: &str, end: &str, interval: i32) -> ScheduleRule {
        ScheduleRule {
            days: days.iter().map(|day| (*day).to_owned()).collect(),
            description: None,
            end_time: end.to_owned(),
            interval,
            start_time: start.to_owned(),
        }
    }

    #[test]
    fn validates_sample_schedule() {
        let schedule = vec![
            rule(&["weekdays"], "09:00", "16:00", 120),
            rule(&["weekends"], "00:00", "23:59", 3_600),
        ];
        validate_schedule(&schedule).expect("sample schedule must validate");
    }

    #[test]
    fn switches_interval_at_schedule_boundary() {
        let schedule = vec![
            rule(&["weekdays"], "09:00", "16:00", 120),
            rule(&["weekdays"], "16:00", "23:59", 3_600),
        ];
        let date = Date::from_calendar_date(2026, Month::July, 28).expect("date must build");
        let now = PrimitiveDateTime::new(date, time::Time::from_hms(15, 59, 0).unwrap())
            .assume_offset(UtcOffset::from_whole_seconds(8 * 3_600).unwrap());
        assert_eq!(
            next_delay_seconds(Some(&schedule), 60, now, now.offset()),
            60
        );
    }

    #[test]
    fn handles_a_window_that_crosses_midnight() {
        let schedule = vec![rule(&["monday"], "23:00", "02:00", 120)];
        let date = Date::from_calendar_date(2026, Month::July, 28).expect("date must build");
        let now = PrimitiveDateTime::new(date, time::Time::from_hms(1, 59, 0).unwrap())
            .assume_offset(UtcOffset::from_whole_seconds(8 * 3_600).unwrap());
        assert_eq!(
            next_delay_seconds(Some(&schedule), 3_600, now, now.offset()),
            60
        );
    }
}

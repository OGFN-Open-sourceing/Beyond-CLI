//! `/later` reminder engine: natural-language scheduling, persistent store,
//! and firing via Windows toast notifications using the user's local timezone.

use anyhow::{bail, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Weekday};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    /// Local time the reminder should fire.
    pub when: DateTime<Local>,
    pub task: String,
    pub created: DateTime<Local>,
    #[serde(default)]
    pub fired: bool,
}

/// Schedule a reminder from natural language + store it.
pub fn schedule(when: &str, task: &str) -> Result<DateTime<Local>> {
    let dt = parse_when(when)?;
    let mut list = load_all();
    list.push(Reminder {
        id: uuid::Uuid::new_v4().to_string()[..8].into(),
        when: dt,
        task: task.to_string(),
        created: Local::now(),
        fired: false,
    });
    save(&list)?;
    Ok(dt)
}

/// List reminders (oldest first).
pub fn list() -> Vec<Reminder> {
    let mut v = load_all();
    v.sort_by_key(|r| r.when);
    v
}

/// Remove all reminders; returns how many were removed.
pub fn clear() -> usize {
    let n = load_all().len();
    let _ = save(&[]);
    n
}

/// Mark a reminder as fired (called once its toast has been shown).
pub fn mark_fired(id: &str) {
    let mut all = load_all();
    if let Some(r) = all.iter_mut().find(|r| r.id == id) {
        r.fired = true;
    }
    let _ = save(&all);
}

/// Drop fired reminders older than 24h to keep the store lean.
pub fn prune() {
    let cutoff = Local::now() - Duration::hours(24);
    let kept: Vec<Reminder> = load_all()
        .into_iter()
        .filter(|r| !r.fired || r.when > cutoff)
        .collect();
    let _ = save(&kept);
}

fn store_file() -> PathBuf {
    crate::config::Paths::reminders_file()
}

fn load_all() -> Vec<Reminder> {
    std::fs::read_to_string(store_file())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save(v: &[Reminder]) -> Result<()> {
    std::fs::write(store_file(), serde_json::to_string_pretty(v)?)?;
    Ok(())
}

// ------------------------------------------------------------- time parsing

/// Parse a human "when" into a local DateTime.
///
/// Supported styles (all resolved in the user's local timezone):
/// - relative: `in 2h`, `in 30m`, `in 2h30m`, `in 45s`
/// - `tomorrow`, `tomorrow 9am`, `tomorrow 18:30`
/// - `tonight` (= 20:00), `now+1h`
/// - `next week`, `next monday`, `friday 18:00`
/// - clock only: `18:00`, `9am`, `9pm` (today, or tomorrow if already past)
/// - ISO: `2026-11-20 14:30`
pub fn parse_when(s: &str) -> Result<DateTime<Local>> {
    let s = s.trim().to_lowercase();
    let now = Local::now();

    // ISO date[-time]
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M") {
        return local_from_naive(dt);
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
        return local_from_naive(d.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap()));
    }

    // in Xh / Xm / Xh Ym
    if let Some(rest) = s.strip_prefix("in ") {
        if let Some(dur) = parse_duration(rest.trim()) {
            return Ok(now + dur);
        }
        bail!("could not parse duration '{rest}' (try 'in 2h30m')");
    }
    if let Some(rest) = s.strip_prefix("now+")
        && let Some(dur) = parse_duration(rest.trim()) {
            return Ok(now + dur);
        }

    // tomorrow / tonight / next week / weekday forms
    let (mut day, mut rest) = (now.date_naive(), s.as_str());
    let mut day_offset: Option<i64> = None;

    if let Some(r) = s.strip_prefix("tomorrow") {
        day_offset = Some(1);
        rest = r.trim();
    } else if s.starts_with("tonight") {
        day_offset = Some(0);
        rest = s.strip_prefix("tonight").unwrap().trim();
        if rest.is_empty() {
            rest = "20:00";
        }
    } else if let Some(r) = s.strip_prefix("next week") {
        day_offset = Some(7);
        rest = r.trim();
    } else if let Some((wd, r)) = parse_weekday(&s) {
        // e.g. "friday 18:00" / "next friday 9am"
        let days_until = (wd.num_days_from_monday() + 7 - now.weekday().num_days_from_monday()) % 7;
        day_offset = Some(if days_until == 0 { 7 } else { days_until as i64 });
        let r = r.trim();
        rest = r.strip_prefix("next").unwrap_or(r).trim();
    }

    if let Some(off) = day_offset {
        day += chrono::Duration::days(off);
        let time = if rest.is_empty() {
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        } else {
            parse_clock(rest)?
        };
        return local_from_naive(day.and_time(time));
    }

    // bare clock time: today, or tomorrow if it already passed
    let time = parse_clock(&s)?;
    let dt = now.date_naive().and_time(time);
    let cand = local_from_naive(dt)?;
    if cand <= now {
        return local_from_naive(now.date_naive().and_time(time) + chrono::Duration::days(1));
    }
    Ok(cand)
}

fn local_from_naive(dt: chrono::NaiveDateTime) -> Result<DateTime<Local>> {
    Local
        .from_local_datetime(&dt)
        .single()
        .ok_or_else(|| anyhow::anyhow!("ambiguous local time"))
}

/// `2h`, `90m`, `2h30m`, `45s`
fn parse_duration(s: &str) -> Option<Duration> {
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut matched = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else if c.is_alphabetic() {
            let n: i64 = num.parse().ok()?;
            num.clear();
            match c {
                'h' => total += Duration::hours(n),
                'm' => total += Duration::minutes(n),
                's' => total += Duration::seconds(n),
                _ => return None,
            }
            matched = true;
        } else if c != ' ' {
            return None;
        }
    }
    matched.then_some(total)
}

/// `18:30`, `9am`, `9pm`, `9:30am`
fn parse_clock(s: &str) -> Result<NaiveTime> {
    let s = s.trim();
    let (hms, ampm): (&str, bool) = if let Some(r) = s.strip_suffix("am") {
        (r.trim(), true)
    } else if let Some(r) = s.strip_suffix("pm") {
        (r.trim(), true)
    } else {
        (s, false)
    };
    let is_pm = s.ends_with("pm");
    let mut parts = hms.split(':');
    let h: i64 = parts.next().unwrap_or("").trim().parse()?;
    let m: i64 = parts.next().unwrap_or("0").trim().parse()?;
    let mut h = h;
    if ampm {
        if is_pm && h < 12 {
            h += 12;
        }
        if !is_pm && h == 12 {
            h = 0;
        }
    }
    NaiveTime::from_hms_opt(h.clamp(0, 23) as u32, m.clamp(0, 59) as u32, 0)
        .ok_or_else(|| anyhow::anyhow!("bad time '{s}'"))
}

/// Find a leading weekday name; returns (weekday, remainder).
fn parse_weekday(s: &str) -> Option<(Weekday, &str)> {
    let names = [
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
    ];
    for (name, wd) in names.iter() {
        // also match "mon", "tue"…
        if let Some(rest) = s.strip_prefix(name) {
            return Some((*wd, rest));
        }
        if let Some(short) = name.get(..3)
            && let Some(rest) = s.strip_prefix(short)
        {
            return Some((*wd, rest));
        }
    }
    None
}

// ---------------------------------------------------------------- firing

/// Fire a reminder now: Windows toast + mark as fired. Returns the task text.
/// (Blocking: intended to be called from `spawn_blocking`.)
pub fn fire(r: &Reminder) -> Result<String> {
    let _ = crate::util::send_toast("Beyond CLI — reminder", &r.task);
    mark_fired(&r.id);
    Ok(r.task.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("2h"), Some(Duration::hours(2)));
        assert_eq!(parse_duration("2h30m"), Some(Duration::hours(2) + Duration::minutes(30)));
        assert_eq!(parse_duration("45s"), Some(Duration::seconds(45)));
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn clocks() {
        let t = parse_clock("18:30").unwrap();
        assert_eq!(t.format("%H:%M").to_string(), "18:30");
        let t = parse_clock("9pm").unwrap();
        assert_eq!(t.format("%H:%M").to_string(), "21:00");
        let t = parse_clock("12am").unwrap();
        assert_eq!(t.format("%H:%M").to_string(), "00:00");
    }

    #[test]
    fn iso() {
        assert!(parse_when("2026-11-20 14:30").is_ok());
        assert!(parse_when("2026-11-20").is_ok());
    }
}

//! Local wall-clock time (honours the system timezone).

use chrono::{Datelike, Local, Timelike};

pub struct LocalTime {
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
    weekday: i32,
    day: i32,
    month: i32,
}

impl LocalTime {
    /// `Thu, Oct 30` — the same format as the bar.
    pub fn date_label(&self) -> String {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] =
            ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!(
            "{}, {} {}",
            DAYS[self.weekday.rem_euclid(7) as usize],
            MONTHS[self.month.rem_euclid(12) as usize],
            self.day
        )
    }
}

pub fn now() -> LocalTime {
    let t = Local::now();
    LocalTime {
        hour: t.hour() as i32,
        minute: t.minute() as i32,
        second: t.second() as i32,
        weekday: t.weekday().num_days_from_sunday() as i32,
        day: t.day() as i32,
        month: t.month0() as i32,
    }
}

/// `HH:MM:SS` local time of `t`.
pub fn hms(t: std::time::SystemTime) -> String {
    chrono::DateTime::<Local>::from(t).format("%H:%M:%S").to_string()
}

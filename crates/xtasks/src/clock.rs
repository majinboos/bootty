use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use time::OffsetDateTime;
use time::macros::format_description;

pub fn utc_timestamp() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("failed to format UTC timestamp")
}

pub fn utc_datetime() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .context("failed to format UTC time")
}

pub struct Timer(Instant);

impl Timer {
    pub fn start() -> Self {
        Self(Instant::now())
    }

    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

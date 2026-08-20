use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn now() -> u64 {
    OffsetDateTime::now_utc().unix_timestamp().max(0) as u64
}

pub fn rfc3339(timestamp: u64) -> String {
    OffsetDateTime::from_unix_timestamp(timestamp as i64)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| timestamp.to_string())
}

pub fn humanize(seconds: u64) -> String {
    if seconds >= 3600 {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{minutes}m")
        };
    }
    if seconds >= 60 {
        return format!("{}m", seconds / 60);
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_formats_the_epoch() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn durations_render_compactly() {
        assert_eq!(humanize(45), "45s");
        assert_eq!(humanize(120), "2m");
        assert_eq!(humanize(3600), "1h");
        assert_eq!(humanize(5400), "1h30m");
    }

    #[test]
    fn now_is_after_the_year_2020() {
        assert!(now() > 1_577_836_800);
    }
}

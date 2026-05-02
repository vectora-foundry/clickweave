use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde_json::{Value, json};

pub(crate) const TOOL_NAME: &str = "get_current_datetime";

pub(crate) fn current_datetime_json() -> String {
    let now_utc = Utc::now();
    let local = now_utc.with_timezone(&chrono::Local);
    let offset_seconds = local.offset().local_minus_utc();
    let timezone_name = system_timezone_name();
    current_datetime_value(now_utc, offset_seconds, timezone_name).to_string()
}

pub(crate) fn current_datetime_value(
    now_utc: DateTime<Utc>,
    offset_seconds: i32,
    timezone_name: Option<String>,
) -> Value {
    let offset = FixedOffset::east_opt(offset_seconds)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero fixed offset must be valid"));
    let local = now_utc.with_timezone(&offset);

    json!({
        "kind": "current_datetime",
        "source": "system_clock",
        "utc_datetime": now_utc.to_rfc3339_opts(SecondsFormat::Millis, true),
        "local_datetime": local.to_rfc3339_opts(SecondsFormat::Millis, false),
        "unix_millis": now_utc.timestamp_millis(),
        "utc_date": now_utc.format("%Y-%m-%d").to_string(),
        "local_date": local.format("%Y-%m-%d").to_string(),
        "local_time": local.format("%H:%M:%S").to_string(),
        "timezone": {
            "name": timezone_name,
            "offset": format_offset(offset_seconds),
            "offset_seconds": offset_seconds,
        },
    })
}

fn format_offset(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let total_minutes = offset_seconds.unsigned_abs() / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

fn system_timezone_name() -> Option<String> {
    iana_time_zone::get_timezone()
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("TZ").ok().filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn current_datetime_value_uses_injected_clock_and_offset() {
        let now = Utc.with_ymd_and_hms(2026, 5, 2, 10, 30, 45).unwrap();
        let value = current_datetime_value(now, 2 * 60 * 60, Some("Europe/Belgrade".to_string()));

        assert_eq!(value["kind"], "current_datetime");
        assert_eq!(value["source"], "system_clock");
        assert_eq!(value["utc_datetime"], "2026-05-02T10:30:45.000Z");
        assert_eq!(value["local_datetime"], "2026-05-02T12:30:45.000+02:00");
        assert_eq!(value["unix_millis"], 1_777_717_845_000_i64);
        assert_eq!(value["utc_date"], "2026-05-02");
        assert_eq!(value["local_date"], "2026-05-02");
        assert_eq!(value["local_time"], "12:30:45");
        assert_eq!(value["timezone"]["name"], "Europe/Belgrade");
        assert_eq!(value["timezone"]["offset"], "+02:00");
        assert_eq!(value["timezone"]["offset_seconds"], 7200);
    }

    #[test]
    fn current_datetime_value_formats_negative_offsets() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 3, 4, 5).unwrap();
        let value = current_datetime_value(now, -(5 * 60 * 60 + 30 * 60), None);

        assert_eq!(value["local_datetime"], "2026-01-14T21:34:05.000-05:30");
        assert_eq!(value["timezone"]["name"], Value::Null);
        assert_eq!(value["timezone"]["offset"], "-05:30");
        assert_eq!(value["timezone"]["offset_seconds"], -19800);
    }
}

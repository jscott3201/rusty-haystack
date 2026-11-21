use crate::ReadError;
use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, TimeZone};
use haystack_core::{
    codecs::{Codec, zinc::ZincCodec},
    kinds::{HDateTime, Kind, offset_at, resolve_local_offset, tz_for},
};

pub(crate) fn point_time(
    instant: DateTime<FixedOffset>,
    timezone: &str,
) -> Result<HDateTime, ReadError> {
    let offset = offset_at(timezone, instant)
        .ok_or(ReadError::InvalidQuery("unsupported history timezone"))?;
    Ok(HDateTime::new(instant.with_timezone(&offset), timezone))
}
pub(crate) fn validate_zone(zone: &str) -> Result<(), ReadError> {
    if zone == "Rel" || zone.contains('/') || tz_for(zone).is_none() {
        Err(ReadError::InvalidQuery("unsupported H4 history timezone"))
    } else {
        Ok(())
    }
}
fn midnight(date: NaiveDate, zone: &str) -> Result<DateTime<FixedOffset>, ReadError> {
    let local = date
        .and_hms_opt(0, 0, 0)
        .ok_or(ReadError::InvalidQuery("invalid calendar date"))?;
    match resolve_local_offset(zone, local) {
        Some(LocalResult::Single(offset)) => offset
            .from_local_datetime(&local)
            .single()
            .ok_or(ReadError::InvalidQuery("invalid midnight")),
        Some(LocalResult::None | LocalResult::Ambiguous(..)) => Err(ReadError::InvalidQuery(
            "ambiguous or nonexistent calendar midnight",
        )),
        None => Err(ReadError::InvalidQuery("unsupported history timezone")),
    }
}
fn boundary(text: &str, zone: &str, end_date: bool) -> Result<DateTime<FixedOffset>, ReadError> {
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let date = if end_date {
            date.succ_opt()
                .ok_or(ReadError::InvalidQuery("date overflow"))?
        } else {
            date
        };
        return midnight(date, zone);
    }
    let Kind::DateTime(value) = ZincCodec
        .decode_scalar(text)
        .map_err(|_| ReadError::InvalidQuery("invalid history boundary"))?
    else {
        return Err(ReadError::InvalidQuery(
            "history boundary must be Date or DateTime",
        ));
    };
    validate_zone(&value.tz_name)?;
    if offset_at(&value.tz_name, value.dt) != Some(*value.dt.offset()) {
        return Err(ReadError::InvalidQuery(
            "inconsistent boundary timezone offset",
        ));
    }
    // Read boundaries may use a different admitted timezone. Preserve the instant.
    Ok(value.dt)
}
pub(crate) fn parse_range(
    text: &str,
    zone: &str,
    now: DateTime<FixedOffset>,
) -> Result<(HDateTime, HDateTime), ReadError> {
    validate_zone(zone)?;
    let text = text.trim();
    let (start, end) = match text {
        "today" | "yesterday" => {
            let today = point_time(now, zone)?.dt.date_naive();
            let date = if text == "yesterday" {
                today
                    .pred_opt()
                    .ok_or(ReadError::InvalidQuery("date overflow"))?
            } else {
                today
            };
            (
                midnight(date, zone)?,
                midnight(
                    date.succ_opt()
                        .ok_or(ReadError::InvalidQuery("date overflow"))?,
                    zone,
                )?,
            )
        }
        _ => match text.split_once(',') {
            Some((start, end)) => (
                boundary(start.trim(), zone, false)?,
                boundary(end.trim(), zone, true)?,
            ),
            None => {
                let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
                    ReadError::InvalidQuery("single history range must be a calendar date")
                })?;
                (
                    midnight(date, zone)?,
                    midnight(
                        date.succ_opt()
                            .ok_or(ReadError::InvalidQuery("date overflow"))?,
                        zone,
                    )?,
                )
            }
        },
    };
    if start > end {
        return Err(ReadError::InvalidQuery("reversed history range"));
    }
    Ok((point_time(start, zone)?, point_time(end, zone)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2024-06-02T01:00:00Z").unwrap()
    }
    #[test]
    fn calendar_days_follow_local_dst_and_current_date() {
        for (date, hours) in [("2024-03-10", 23), ("2024-11-03", 25)] {
            let (start, end) = parse_range(date, "New_York", now()).unwrap();
            assert_eq!((end.dt - start.dt).num_hours(), hours);
            assert_eq!(start.tz_name, "New_York");
        }
        let (start, end) = parse_range("today", "New_York", now()).unwrap();
        assert_eq!(start.dt.date_naive().to_string(), "2024-06-01");
        assert_eq!(end.dt.date_naive().to_string(), "2024-06-02");
        assert!(DateTime::parse_from_rfc3339("2024-06-02T03:59:59.999999999Z").unwrap() < end.dt);
    }
    #[test]
    fn cross_zone_explicit_instants_and_fractional_seconds_are_preserved() {
        let (start, end) = parse_range(
            "2024-06-01T04:00:00.125Z GMT,2024-06-02T04:00:00.875Z GMT",
            "New_York",
            now(),
        )
        .unwrap();
        assert_eq!(start.dt.to_rfc3339(), "2024-06-01T00:00:00.125-04:00");
        assert_eq!(end.dt.to_rfc3339(), "2024-06-02T00:00:00.875-04:00");
        assert_eq!(start.tz_name, "New_York");
    }
    #[test]
    fn unsupported_and_transition_midnights_are_errors() {
        for zone in ["Rel", "America/New_York", "missing"] {
            assert!(parse_range("today", zone, now()).is_err());
        }
        assert!(parse_range("2011-12-30", "Apia", now()).is_err());
        assert!(parse_range("2015-11-01", "Havana", now()).is_err());
        assert!(
            parse_range(
                "2024-06-01T00:00:00Z New_York,2024-06-02T00:00:00Z New_York",
                "New_York",
                now()
            )
            .is_err()
        );
    }
}

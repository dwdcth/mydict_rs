//! 日期与时区：全项目「一天」的唯一出处 —— 移植自 `app/core/timeutil.py`。
//!
//! 时间戳一律以 unix 秒（UTC）存库，「一天」按部署本地时区划分。按天过滤必须先
//! 在这里把本地日换算成 UTC 半开区间 `[start_unix, end_unix)` 再比较，不能对
//! created_at 套日期函数。

use chrono::{Duration, Local, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz as NamedTz;
use std::sync::OnceLock;

/// 部署本地时区：配置了有效 TIMEZONE 就用命名时区（含夏令时规则），
/// 否则用系统时区 —— 后者只是当前时刻的固定偏移，没有夏令时规则。
#[derive(Debug, Clone, Copy)]
pub enum LocalZone {
    Named(NamedTz),
    Fixed(chrono::FixedOffset),
}

static ZONE: OnceLock<LocalZone> = OnceLock::new();

pub fn local_zone(timezone_setting: &str) -> LocalZone {
    *ZONE.get_or_init(|| {
        let name = timezone_setting.trim();
        if !name.is_empty() {
            if let Ok(tz) = name.parse::<NamedTz>() {
                return LocalZone::Named(tz);
            }
            tracing::warn!(timezone = name, "TIMEZONE 无效，回落到系统时区");
        }
        LocalZone::Fixed(*Local::now().offset())
    })
}

impl LocalZone {
    /// 本地某日零点对应的 UTC unix 秒。夏令时缺口（春季拨快）取 earliest。
    fn local_midnight_to_unix(&self, date: NaiveDate) -> Option<i64> {
        let naive = date.and_hms_opt(0, 0, 0)?;
        match self {
            LocalZone::Named(tz) => tz
                .from_local_datetime(&naive)
                .earliest()
                .map(|dt| dt.timestamp()),
            LocalZone::Fixed(offset) => offset
                .from_local_datetime(&naive)
                .single()
                .map(|dt| dt.timestamp()),
        }
    }
}

pub fn now_local_date(zone: LocalZone) -> NaiveDate {
    match zone {
        LocalZone::Named(tz) => tz.from_utc_datetime(&Utc::now().naive_utc()).date_naive(),
        LocalZone::Fixed(offset) => Utc::now().with_timezone(&offset).date_naive(),
    }
}

/// 本地日的 ISO 字符串（YYYY-MM-DD），offset_days 相对今天偏移
pub fn day_str(zone: LocalZone, offset_days: i64) -> String {
    let d = now_local_date(zone) + Duration::days(offset_days);
    d.format("%Y-%m-%d").to_string()
}

pub fn today_str(zone: LocalZone) -> String {
    day_str(zone, 0)
}

/// 解析 ISO 日期字符串；空值或格式非法返回 None
pub fn parse_day(value: Option<&str>) -> Option<NaiveDate> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

/// 本地日 `date` 对应的 `[start, end)` UTC unix 秒区间
pub fn day_bounds_unix(zone: LocalZone, target: NaiveDate) -> Option<(i64, i64)> {
    let start = zone.local_midnight_to_unix(target)?;
    let next_day = target.succ_opt()?;
    let end = zone.local_midnight_to_unix(next_day)?;
    Some((start, end))
}

/// 本地日闭区间 `[start_day, end_day]` 对应的 `[start, end)` UTC 区间
pub fn range_bounds_unix(
    zone: LocalZone,
    start_day: NaiveDate,
    end_day: NaiveDate,
) -> Option<(i64, i64)> {
    let (start, _) = day_bounds_unix(zone, start_day)?;
    let (_, end) = day_bounds_unix(zone, end_day)?;
    Some((start, end))
}

/// 到下一个本地零点的秒数，用于按日限流的 `Retry-After`
pub fn seconds_to_local_midnight(zone: LocalZone) -> i64 {
    let now_ts = Utc::now().timestamp();
    let today = match zone {
        LocalZone::Named(tz) => tz.from_utc_datetime(&Utc::now().naive_utc()).date_naive(),
        LocalZone::Fixed(offset) => Utc::now().with_timezone(&offset).date_naive(),
    };
    let tomorrow = today.succ_opt().and_then(|d| zone.local_midnight_to_unix(d));
    match tomorrow {
        Some(ts) => (ts - now_ts).max(1),
        None => 1,
    }
}

/// DB 存的 unix 秒 → API 输出的 naive ISO 字符串（秒精度，无时区后缀）。
/// Python 版 DB 生成的时间戳是 SQLite `datetime('now')`（秒精度无小数），
/// 这里保持同一形状；前端用 dayjs 解析两种格式都兼容。
pub fn unix_to_iso(unix: i64) -> String {
    match Utc.timestamp_opt(unix, 0).single() {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shanghai() -> LocalZone {
        LocalZone::Named(chrono_tz::Asia::Shanghai)
    }

    #[test]
    fn shanghai_day_bounds_are_utc_plus_8() {
        let zone = shanghai();
        let date = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let (start, end) = day_bounds_unix(zone, date).unwrap();
        // 上海 2026-10-05 00:00 = UTC 2026-10-04 16:00
        assert_eq!(unix_to_iso(start), "2026-10-04T16:00:00");
        assert_eq!(end - start, 86400);
    }

    #[test]
    fn parse_day_rejects_garbage() {
        assert_eq!(parse_day(Some("2026-10-05")), NaiveDate::from_ymd_opt(2026, 10, 5));
        assert_eq!(parse_day(Some("not-a-date")), None);
        assert_eq!(parse_day(None), None);
        assert_eq!(parse_day(Some("")), None);
    }

    #[test]
    fn day_offset_wraps_month() {
        let zone = shanghai();
        // 不依赖今天具体日期，只验证 day_str 能格式化且偏移一天日期差为 1
        let a = parse_day(Some(&day_str(zone, 0))).unwrap();
        let b = parse_day(Some(&day_str(zone, 1))).unwrap();
        assert_eq!((b - a).num_days(), 1);
    }

    #[test]
    fn midnight_retry_is_positive() {
        assert!(seconds_to_local_midnight(shanghai()) >= 1);
    }
}

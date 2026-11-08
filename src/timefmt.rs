//! 时间序列化：统一输出 RFC 3339（UTC，微秒精度），与 `date-time` 契约一致。

use std::time::{Duration, SystemTime};

use serde::Serializer;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 把 `SystemTime` 序列化为 RFC 3339 字符串。
///
/// 早于 Unix 纪元的时间不合法，序列化为 `None` 会让 JSON 结构变化，
/// 因此直接写入错误字符串以外的最小可用值：1970-01-01T00:00:00Z。
pub fn system_time_as_rfc3339<S>(value: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let text = format_rfc3339(*value);
    serializer.serialize_str(&text)
}

/// 把 `SystemTime` 排序格式化为 RFC 3339；用于响应头和日志。
pub fn format_rfc3339(value: SystemTime) -> String {
    let duration = value
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let seconds = duration.as_secs().min(i64::MAX as u64) as i64;
    let nanos = duration.subsec_nanos();
    let datetime = OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|dt| dt.replace_nanosecond(nanos).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    datetime
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// 解析 RFC 3339 到 `SystemTime`；用于配置与测试。
pub fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    let datetime = OffsetDateTime::parse(value, &Rfc3339).ok()?;
    let nanos = datetime.unix_timestamp_nanos();
    if nanos < 0 {
        return None;
    }
    let seconds = (nanos / 1_000_000_000) as u64;
    let sub = (nanos % 1_000_000_000) as u32;
    Some(SystemTime::UNIX_EPOCH + Duration::new(seconds, sub))
}

/// HTTP 日期头（`Date`），供响应使用。
pub fn http_date(value: SystemTime) -> String {
    let duration = value
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let datetime = OffsetDateTime::from_unix_timestamp(duration.as_secs() as i64)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    datetime
        .format(&time::format_description::well_known::Rfc2822)
        .unwrap_or_else(|_| "Thu, 01 Jan 1970 00:00:00 +0000".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_epoch() {
        assert_eq!(
            format_rfc3339(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn round_trips_microseconds() {
        let value = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_000);
        let text = format_rfc3339(value);
        let parsed = parse_rfc3339(&text).unwrap();
        assert_eq!(parsed, value);
    }
}

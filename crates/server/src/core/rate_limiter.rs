//! 每分钟限流计数器 —— 移植自 `app/core/rate_limiter.py`。
//!
//! 固定墙钟分钟窗（epoch // 60），进程内 TTL 缓存，**先加后判**（第 N ≤ limit 次放行）。

use moka::sync::Cache as MokaCache;
use std::time::Duration;

pub struct MinuteCounters {
    inner: MokaCache<String, i64>,
}

impl MinuteCounters {
    pub fn new() -> Self {
        Self {
            inner: MokaCache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_secs(70))
                .build(),
        }
    }

    fn minute_bucket() -> i64 {
        chrono::Utc::now().timestamp() / 60
    }

    /// 计数 +1 并返回是否放行（count <= limit）。
    /// moka 的 entry 原子 upsert（key 级锁串行）保证并发计数正确。
    pub fn check_and_increment(&self, counter_key: &str, limit: i64) -> (bool, i64) {
        let minute = Self::minute_bucket();
        let key = format!("{counter_key}:{minute}");
        let count = self
            .inner
            .entry(key)
            .and_upsert_with(|maybe_entry| match maybe_entry {
                Some(entry) => entry.into_value().saturating_add(1),
                None => 1,
            })
            .into_value();
        (count <= limit, count)
    }

    /// 到下一个整分钟的秒数（Retry-After 用）
    pub fn seconds_to_next_minute() -> i64 {
        let now = chrono::Utc::now().timestamp();
        (60 - now % 60).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_window_allows_up_to_limit() {
        let counters = MinuteCounters::new();
        for i in 1..=3 {
            let (allowed, _) = counters.check_and_increment("t", 3);
            assert!(allowed, "第 {i} 次应放行");
        }
        let (allowed, count) = counters.check_and_increment("t", 3);
        assert!(!allowed, "第 4 次应拒绝");
        assert_eq!(count, 4);
    }

    #[test]
    fn separate_keys_independent() {
        let counters = MinuteCounters::new();
        assert!(counters.check_and_increment("a", 1).0);
        assert!(!counters.check_and_increment("a", 1).0);
        assert!(counters.check_and_increment("b", 1).0);
    }
}

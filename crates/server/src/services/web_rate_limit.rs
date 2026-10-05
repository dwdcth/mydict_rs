//! Web 端按 IP 每分钟限流 —— 移植自 `app/services/web_rate_limit_service.py`。
//!
//! 三档独立计数桶：
//! - 查询（/dict/search、/dict/random 共用）：key `anon:{ip}` / `user:{ip}`，超限写一条
//!   rate_limited 查询日志
//! - 词条文档（/dict/entry）：key `entry:{...}`，限额 = 查询限额 ×10，超限**不写**日志
//! - 在线词典：key `online:{...}`，限额 = max(5, 查询限额/2)，不写日志


use crate::core::errors::AppError;
use crate::core::rate_limiter::MinuteCounters;
use crate::services::query_log;
use crate::AppState;

const ENTRY_RATE_MULTIPLIER: i64 = 10;
const ONLINE_RATE_MIN: i64 = 5;
const ONLINE_RATE_DIVISOR: i64 = 2;

fn anon_key(ip: &str) -> String {
    format!("anon:{ip}")
}

fn user_key(ip: &str) -> String {
    format!("user:{ip}")
}

async fn per_minute_limit(state: &AppState, logged_in: bool) -> Result<i64, AppError> {
    let (key, default) = if logged_in {
        ("user_ip_rate_limit_per_min", state.cfg.user_ip_rate_limit_per_min)
    } else {
        ("anonymous_ip_rate_limit_per_min", state.cfg.anonymous_ip_rate_limit_per_min)
    };
    Ok(crate::services::settings_service::get_int_setting(&state.db, key, default).await?)
}

/// 查询类（search/random）：超限写 rate_limited 查询日志再抛 429
pub async fn enforce_search_rate(
    state: &AppState,
    logged_in: bool,
    ip: &str,
    user_id: Option<i32>,
) -> Result<(), AppError> {
    let limit = per_minute_limit(state, logged_in).await?;
    let counter_key = if logged_in { user_key(ip) } else { anon_key(ip) };
    let (allowed, _) = state.minute_counters.check_and_increment(&counter_key, limit);
    if allowed {
        return Ok(());
    }
    query_log::log_query(
        state,
        query_log::NewQueryLog {
            source: "web",
            token_id: None,
            user_id,
            word: String::new(),
            dictionary_id: None,
            ip: Some(ip.to_string()),
            status: Some("rate_limited"),
            duration_ms: None,
        },
    )
    .await;
    Err(AppError::rate_limited(
        "查询过于频繁，请稍后再试",
        crate::core::rate_limiter::MinuteCounters::seconds_to_next_minute(),
    ))
}

/// 词条文档：独立桶 ×10，超限不写日志（防污染统计）
pub async fn enforce_entry_rate(
    state: &AppState,
    logged_in: bool,
    ip: &str,
) -> Result<(), AppError> {
    let limit = per_minute_limit(state, logged_in).await? * ENTRY_RATE_MULTIPLIER;
    let base = if logged_in { user_key(ip) } else { anon_key(ip) };
    let counter_key = format!("entry:{base}");
    let (allowed, _) = state.minute_counters.check_and_increment(&counter_key, limit);
    if allowed {
        Ok(())
    } else {
        Err(AppError::rate_limited(
            "词条加载过于频繁，请稍后再试",
            MinuteCounters::seconds_to_next_minute(),
        ))
    }
}

/// 在线词典：max(5, 半)，不写日志
pub async fn enforce_online_rate(
    state: &AppState,
    logged_in: bool,
    ip: &str,
) -> Result<(), AppError> {
    let base_limit = per_minute_limit(state, logged_in).await?;
    let limit = std::cmp::max(ONLINE_RATE_MIN, base_limit / ONLINE_RATE_DIVISOR);
    let base = if logged_in { user_key(ip) } else { anon_key(ip) };
    let counter_key = format!("online:{base}");
    let (allowed, _) = state.minute_counters.check_and_increment(&counter_key, limit);
    if allowed {
        Ok(())
    } else {
        Err(AppError::rate_limited(
            "在线词典查询过于频繁，请稍后再试",
            MinuteCounters::seconds_to_next_minute(),
        ))
    }
}

/// v1 API 匿名限流（对齐 Python v1/query.py 的裸 ip key——与 Web 的 anon:{ip} 不共用，
/// 保持原口径；超限写 rate_limited 日志）
pub async fn enforce_api_anonymous_rate(state: &AppState, ip: &str) -> Result<(), AppError> {
    let limit = crate::services::settings_service::get_int_setting(
        &state.db,
        "anonymous_ip_rate_limit_per_min",
        state.cfg.anonymous_ip_rate_limit_per_min,
    )
    .await?;
    let (allowed, _) = state.minute_counters.check_and_increment(ip, limit);
    if allowed {
        return Ok(());
    }
    query_log::log_query(
        state,
        query_log::NewQueryLog {
            source: "api",
            token_id: None,
            user_id: None,
            word: String::new(),
            dictionary_id: None,
            ip: Some(ip.to_string()),
            status: Some("rate_limited"),
            duration_ms: None,
        },
    )
    .await;
    Err(AppError::rate_limited(
        "匿名调用过于频繁，请稍后再试",
        MinuteCounters::seconds_to_next_minute(),
    ))
}

//! Token 每日限流 —— 移植自 `app/services/rate_limit_service.py`。
//!
//! 计数器 = token_usage_daily 当日行的 query_count（本地时区日界，跨重启持久）。
//! 语义（对齐 Python）：先读计数，放行则 query_count+1；已达上限则 rate_limited_count+1
//! 并拒绝，Retry-After = 到本地零点的秒数。

use sea_orm::{ConnectionTrait, Statement};

use crate::core::errors::AppError;
use crate::core::timeutil;
use crate::services::settings_service;
use crate::AppState;

pub async fn check_and_increment_token_daily(
    state: &AppState,
    token_daily_limit: Option<i64>,
    token_id: i32,
) -> Result<(), AppError> {
    let zone = timeutil::local_zone(&state.cfg.timezone);
    let today = timeutil::today_str(zone);
    let limit = match token_daily_limit {
        Some(limit) => limit,
        None => {
            settings_service::get_int_setting(
                &state.db,
                "token_default_daily_limit",
                state.cfg.token_default_daily_limit,
            )
            .await?
        }
    };

    // 原子 UPSERT：query_count+1 并取回新值（SQLite RETURNING；PG 同）
    let backend = state.db.get_database_backend();
    let sql = match backend {
        sea_orm::DbBackend::MySql => {
            // MySQL 无 RETURNING：先原子自增，再回读
            state
                .db
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO token_usage_daily (stat_date, token_id, query_count, rate_limited_count) \
                     VALUES ($1, $2, 1, 0) \
                     ON DUPLICATE KEY UPDATE query_count = query_count + 1",
                    [today.clone().into(), token_id.into()],
                ))
                .await?;
            "SELECT query_count FROM token_usage_daily WHERE stat_date = $1 AND token_id = $2"
                .to_string()
        }
        _ => {
            "INSERT INTO token_usage_daily (stat_date, token_id, query_count, rate_limited_count) \
             VALUES ($1, $2, 1, 0) \
             ON CONFLICT(stat_date, token_id) DO UPDATE SET query_count = query_count + 1 \
             RETURNING query_count"
                .to_string()
        }
    };
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            sql,
            [today.clone().into(), token_id.into()],
        ))
        .await?;
    let count: i64 = row
        .and_then(|r| r.try_get("", "query_count").ok())
        .unwrap_or(1);

    if count > limit {
        // 达上限：本条不计数为查询，计一次被限流；SQLite 的 RETURNING 自增已经发生，
        // 回冲 query_count 并 bump rate_limited_count
        state
            .db
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE token_usage_daily \
                 SET query_count = query_count - 1, rate_limited_count = rate_limited_count + 1 \
                 WHERE stat_date = $1 AND token_id = $2",
                [today.into(), token_id.into()],
            ))
            .await?;
        let retry = timeutil::seconds_to_local_midnight(zone);
        return Err(AppError::rate_limited(
            format!("今日查询量已达上限（{limit} 次），明天再来吧"),
            retry,
        ));
    }
    Ok(())
}

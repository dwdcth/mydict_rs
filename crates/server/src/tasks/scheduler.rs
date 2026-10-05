//! 定时任务调度 —— 移植自 `app/tasks/scheduler.py` + `stats_aggregation.py`，
//! 外加 Rust 版新增的 query_logs 保留期清理。
//!
//! 聚合：每 10 分钟跑一次（启动立即跑），补昨天 + 今天；token 维度由限流实时写
//! token_usage_daily，跳过避免双计。日志清理：每日一次。

use std::sync::Arc;

use crate::core::timeutil::{self, local_zone};
use crate::AppState;

pub fn start(state: &Arc<AppState>) {
    let state = state.clone();
    tokio::spawn(async move {
        // 每 10 分钟聚合（启动立即跑一次，对齐 Python next_run_time=now）
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(600));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut day_ticker = tokio::time::interval(std::time::Duration::from_secs(3600 * 6));
        day_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // day 检查按小时粒度轮询（到点触发），避免长时间空闲进程漂移
        ticker.tick().await; // 第一次立即返回
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(err) = aggregate_current(&state).await {
                        tracing::error!(error = %err, "统计聚合失败");
                    }
                }
                _ = day_ticker.tick() => {
                    if let Err(err) = cleanup_query_logs(&state).await {
                        tracing::error!(error = %err, "查询日志清理失败");
                    }
                }
            }
        }
    });
}

/// 补昨天 + 今天的聚合（重启漏算兜底）
async fn aggregate_current(state: &Arc<AppState>) -> Result<(), sea_orm::DbErr> {
    let zone = local_zone(&state.cfg.timezone);
    for offset in [-1, 0] {
        let date_str = timeutil::day_str(zone, offset);
        aggregate_date(state, &date_str).await?;
    }
    Ok(())
}

/// 把某本地日的 query_logs（非 token 维度）聚合覆盖写进 stats_daily。
/// 对齐 Python：只聚合 token_id IS NULL 的行——token 维度由限流实时写入。
pub async fn aggregate_date(state: &Arc<AppState>, stat_date: &str) -> Result<(), sea_orm::DbErr> {
    let zone = local_zone(&state.cfg.timezone);
    let Some(date) = crate::core::timeutil::parse_day(Some(stat_date)) else {
        return Ok(());
    };
    let Some((start, end)) = crate::core::timeutil::day_bounds_unix(zone, date) else {
        return Ok(());
    };
    let backend = state.db.get_database_backend();
    use sea_orm::ConnectionTrait;
    let rows = state
        .db
        .query_all_raw(sea_orm::Statement::from_string(
            backend,
            format!(
                "SELECT user_id, \
                        SUM(CASE WHEN status = 'rate_limited' THEN 0 ELSE 1 END) AS q, \
                        SUM(CASE WHEN status = 'rate_limited' THEN 1 ELSE 0 END) AS r \
                 FROM query_logs \
                 WHERE token_id IS NULL AND created_at >= {start} AND created_at < {end} \
                 GROUP BY user_id"
            ),
        ))
        .await?;

    // 先清该日聚合行再写（覆盖语义）
    state
        .db
        .execute_raw(sea_orm::Statement::from_sql_and_values(
            backend,
            "DELETE FROM stats_daily WHERE stat_date = $1",
            [stat_date.into()],
        ))
        .await?;
    for row in &rows {
        let user_id: Option<i32> = row.try_get::<Option<i32>>("", "user_id").ok().flatten();
        let q: i64 = row.try_get("", "q").unwrap_or(0);
        let r: i64 = row.try_get("", "r").unwrap_or(0);
        state
            .db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                backend,
                "INSERT INTO stats_daily (stat_date, owner_kind, user_id, query_count, rate_limited_count) \
                 VALUES ($1, $2, $3, $4, $5)",
                [
                    stat_date.into(),
                    if user_id.is_some() { "user" } else { "anon" }.into(),
                    user_id.into(),
                    q.into(),
                    r.into(),
                ],
            ))
            .await?;
    }
    Ok(())
}

/// query_logs 保留期清理（Rust 版新增；空 = 永久保留）
pub async fn cleanup_query_logs(state: &Arc<AppState>) -> Result<u64, sea_orm::DbErr> {
    let days = crate::services::settings_service::get_setting(
        &state.db,
        "query_log_retention_days",
        None,
    )
    .await?
    .and_then(|v| v.trim().parse::<i64>().ok())
    .unwrap_or(0);
    if days <= 0 {
        return Ok(0);
    }
    let zone = local_zone(&state.cfg.timezone);
    let cutoff_date = crate::core::timeutil::parse_day(Some(&timeutil::day_str(zone, -days))).unwrap();
    let Some((_, cutoff)) = crate::core::timeutil::day_bounds_unix(zone, cutoff_date) else {
        return Ok(0);
    };
    use sea_orm::ConnectionTrait;
    let result = state
        .db
        .execute_raw(sea_orm::Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "DELETE FROM query_logs WHERE created_at < $1",
            [cutoff.into()],
        ))
        .await?;
    let removed = result.rows_affected();
    if removed > 0 {
        tracing::info!(removed, cutoff, "清理保留期外的查询日志");
    }
    Ok(removed)
}

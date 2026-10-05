//! 统计 —— 移植自 `app/services/stats_service.py`。
//!
//! 拆表适配：token 维度读 token_usage_daily；user/anon 维度读 stats_daily；
//! date 维度 = 两表之和（Python 版 token 行也在同一张表里，总数语义一致）；
//! source 维度两表都没有，直查 query_logs。

use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::core::timeutil::{self, local_zone, parse_day};
use crate::AppState;

pub async fn get_overview(state: &AppState) -> Result<Value, AppError> {
    let zone = local_zone(&state.cfg.timezone);
    let today = parse_day(Some(&timeutil::today_str(zone))).unwrap();
    let Some((start, end)) = timeutil::day_bounds_unix(zone, today) else {
        return Ok(json!({
            "today_query_count": 0, "active_tokens": 0, "active_users": 0, "dictionary_count": 0
        }));
    };
    // 直接数 query_logs（不等等定时聚合）保证概览实时
    let backend = state.db.get_database_backend();
    let one = |sql: String| async {
        state
            .db
            .query_one_raw(Statement::from_string(backend, sql))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<i64>("", "n").ok())
            .unwrap_or(0)
    };
    let today_query_count = one(format!(
        "SELECT COUNT(*) AS n FROM query_logs WHERE created_at >= {start} AND created_at < {end} AND status != 'rate_limited'"
    ))
    .await;
    let active_tokens = one(format!(
        "SELECT COUNT(DISTINCT token_id) AS n FROM query_logs WHERE created_at >= {start} AND created_at < {end} AND token_id IS NOT NULL"
    ))
    .await;
    let active_users = one(format!(
        "SELECT COUNT(DISTINCT user_id) AS n FROM query_logs WHERE created_at >= {start} AND created_at < {end} AND user_id IS NOT NULL"
    ))
    .await;
    let dictionary_count = one("SELECT COUNT(*) AS n FROM dictionaries".to_string()).await;
    Ok(json!({
        "today_query_count": today_query_count,
        "active_tokens": active_tokens,
        "active_users": active_users,
        "dictionary_count": dictionary_count,
    }))
}

/// 日期参数缺省取近 7 天；给了但格式非法返回 None（按「查不到数据」处理）
fn default_range(
    zone: timeutil::LocalZone,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> Option<(chrono::NaiveDate, chrono::NaiveDate)> {
    let start_parsed = start_date.and_then(|s| parse_day(Some(s)));
    let end_parsed = end_date.and_then(|s| parse_day(Some(s)));
    if start_date.is_some_and(|_s| start_parsed.is_none()) || end_date.is_some_and(|_s| end_parsed.is_none()) {
        return None;
    }
    let start = start_parsed
        .or_else(|| parse_day(Some(&timeutil::day_str(zone, -6))))?;
    let end = end_parsed.or_else(|| parse_day(Some(&timeutil::today_str(zone))))?;
    Some((start, end))
}

pub async fn query_dimension_stats(
    state: &AppState,
    dimension: &str,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> Result<Vec<Value>, AppError> {
    if !["token", "user", "date", "source"].contains(&dimension) {
        return Err(AppError::validation(format!("不支持的统计维度：{dimension}")));
    }
    let zone = local_zone(&state.cfg.timezone);
    let Some((start, end)) = default_range(zone, start_date, end_date) else {
        return Ok(Vec::new());
    };
    let (start_str, end_str) = (
        start.format("%Y-%m-%d").to_string(),
        end.format("%Y-%m-%d").to_string(),
    );
    let backend = state.db.get_database_backend();

    let rows = match dimension {
        "token" => {
            let rows = state
                .db
                .query_all_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT t.id, t.name, COALESCE(SUM(u.query_count), 0) AS q, COALESCE(SUM(u.rate_limited_count), 0) AS r \
                     FROM api_tokens t \
                     LEFT JOIN token_usage_daily u ON u.token_id = t.id \
                       AND u.stat_date >= $1 AND u.stat_date <= $2 \
                     GROUP BY t.id, t.name \
                     ORDER BY q DESC",
                    [start_str.clone().into(), end_str.clone().into()],
                ))
                .await?;
            rows.iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<i32>("", "id").ok(),
                        "label": r.try_get::<String>("", "name").unwrap_or_default(),
                        "query_count": r.try_get::<i64>("", "q").unwrap_or(0),
                        "rate_limited_count": r.try_get::<i64>("", "r").unwrap_or(0),
                    })
                })
                .collect()
        }
        "user" => {
            let rows = state
                .db
                .query_all_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT u.id, u.username, COALESCE(SUM(s.query_count), 0) AS q, COALESCE(SUM(s.rate_limited_count), 0) AS r \
                     FROM users u \
                     LEFT JOIN stats_daily s ON s.user_id = u.id \
                       AND s.stat_date >= $1 AND s.stat_date <= $2 \
                     GROUP BY u.id, u.username \
                     ORDER BY q DESC",
                    [start_str.clone().into(), end_str.clone().into()],
                ))
                .await?;
            rows.iter()
                .map(|r| {
                    json!({
                        "id": r.try_get::<i32>("", "id").ok(),
                        "label": r.try_get::<String>("", "username").unwrap_or_default(),
                        "query_count": r.try_get::<i64>("", "q").unwrap_or(0),
                        "rate_limited_count": r.try_get::<i64>("", "r").unwrap_or(0),
                    })
                })
                .collect()
        }
        "date" => {
            // date 维度 = 聚合表 + 限流计数表之和（原版 token 行也在同一表）
            let rows = state
                .db
                .query_all_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT stat_date, SUM(query_count) AS q, SUM(rate_limited_count) AS r FROM ( \
                       SELECT stat_date, SUM(query_count) AS query_count, SUM(rate_limited_count) AS rate_limited_count \
                       FROM stats_daily WHERE stat_date >= $1 AND stat_date <= $2 GROUP BY stat_date \
                       UNION ALL \
                       SELECT stat_date, SUM(query_count), SUM(rate_limited_count) \
                       FROM token_usage_daily WHERE stat_date >= $1 AND stat_date <= $2 GROUP BY stat_date \
                     ) GROUP BY stat_date ORDER BY stat_date",
                    [start_str.clone().into(), end_str.clone().into()],
                ))
                .await?;
            rows.iter()
                .map(|r| {
                    json!({
                        "id": Value::Null,
                        "label": r.try_get::<String>("", "stat_date").unwrap_or_default(),
                        "query_count": r.try_get::<i64>("", "q").unwrap_or(0),
                        "rate_limited_count": r.try_get::<i64>("", "r").unwrap_or(0),
                    })
                })
                .collect()
        }
        _ => {
            // source：直查 query_logs（本地日区间 → UTC 半开区间）
            let Some((range_start, range_end)) =
                timeutil::range_bounds_unix(zone, start, end)
            else {
                return Ok(Vec::new());
            };
            let rows = state
                .db
                .query_all_raw(Statement::from_string(
                    backend,
                    format!(
                        "SELECT source, \
                                SUM(CASE WHEN status != 'rate_limited' THEN 1 ELSE 0 END) AS q, \
                                SUM(CASE WHEN status = 'rate_limited' THEN 1 ELSE 0 END) AS r \
                         FROM query_logs WHERE created_at >= {range_start} AND created_at < {range_end} \
                         GROUP BY source"
                    ),
                ))
                .await?;
            rows.iter()
                .map(|r| {
                    json!({
                        "id": Value::Null,
                        "label": r.try_get::<String>("", "source").unwrap_or_default(),
                        "query_count": r.try_get::<i64>("", "q").unwrap_or(0),
                        "rate_limited_count": r.try_get::<i64>("", "r").unwrap_or(0),
                    })
                })
                .collect()
        }
    };
    Ok(rows)
}

pub async fn top_words(
    state: &AppState,
    start_date: Option<&str>,
    end_date: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, AppError> {
    let zone = local_zone(&state.cfg.timezone);
    let Some((start, end)) = default_range(zone, start_date, end_date) else {
        return Ok(Vec::new());
    };
    let Some((range_start, range_end)) = timeutil::range_bounds_unix(zone, start, end) else {
        return Ok(Vec::new());
    };
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT word, COUNT(*) AS c FROM query_logs \
             WHERE created_at >= $1 AND created_at < $2 AND status != 'rate_limited' \
             GROUP BY word ORDER BY c DESC LIMIT $3",
            [range_start.into(), range_end.into(), limit.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "word": r.try_get::<String>("", "word").unwrap_or_default(),
                "count": r.try_get::<i64>("", "c").unwrap_or(0),
            })
        })
        .collect())
}

/// 生成 CSV（Python csv 模块默认 \r\n 行终止，逐字节对齐）
pub fn stats_rows_to_csv(rows: &[Value]) -> String {
    let mut out = String::from("id,label,query_count,rate_limited_count\r\n");
    for row in rows {
        let id = match &row["id"] {
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let label = csv_escape(row["label"].as_str().unwrap_or_default());
        out.push_str(&format!(
            "{id},{label},{},{}\r\n",
            row["query_count"], row["rate_limited_count"]
        ));
    }
    out
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

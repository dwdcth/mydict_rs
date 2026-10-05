//! 查询日志 —— 移植自 `app/services/query_log_service.py`。

use sea_orm::{ConnectionTrait, Set};
use serde_json::Value;

use crate::core::errors::AppError;
use crate::entities::{query_log, user};
use crate::AppState;

pub struct NewQueryLog {
    pub source: &'static str, // "api" | "web"
    pub token_id: Option<i32>,
    pub user_id: Option<i32>,
    pub word: String,
    pub dictionary_id: Option<i32>,
    pub ip: Option<String>,
    pub status: Option<&'static str>, // success | not_found | rate_limited | error
    pub duration_ms: Option<i32>,
}

pub async fn log_query(state: &AppState, log: NewQueryLog) {
    let record = query_log::ActiveModel {
        source: Set(log.source.to_string()),
        token_id: Set(log.token_id),
        user_id: Set(log.user_id),
        word: Set(log.word),
        dictionary_id: Set(log.dictionary_id),
        ip: Set(log.ip),
        status: Set(log.status.map(|s| s.to_string())),
        duration_ms: Set(log.duration_ms),
        created_at: Set(chrono::Utc::now().timestamp()),
        ..Default::default()
    };
    use sea_orm::EntityTrait;
    if let Err(err) = query_log::Entity::insert(record).exec(&state.db).await {
        // 日志写失败不连累业务响应
        tracing::warn!(error = %err, "写查询日志失败");
    }
}

/// 最近 100 条成功查询历史（登录用户）
pub async fn get_recent_history(
    state: &AppState,
    user_id: i32,
) -> Result<Vec<Value>, AppError> {
    let rows = state
        .db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT q.word, q.dictionary_id, d.name AS dictionary_name, q.created_at \
             FROM query_logs q LEFT JOIN dictionaries d ON d.id = q.dictionary_id \
             WHERE q.user_id = $1 AND q.status = 'success' \
             ORDER BY q.id DESC LIMIT 100",
            [user_id.into()],
        ))
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(serde_json::json!({
            "word": row.try_get::<String>("", "word").unwrap_or_default(),
            "dictionary_id": row.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
            "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
            "created_at": crate::core::timeutil::unix_to_iso(
                row.try_get::<i64>("", "created_at").unwrap_or_default()
            ),
        }));
    }
    Ok(out)
}

#[allow(dead_code)]
async fn user_exists(state: &AppState, user_id: i32) -> bool {
    use sea_orm::EntityTrait;
    user::Entity::find_by_id(user_id)
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .is_some()
}

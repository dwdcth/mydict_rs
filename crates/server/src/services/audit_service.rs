//! 审计日志 —— 移植自 `app/services/audit_service.py`。
//! 仅记录 admin/system 操作；detail 是 ensure_ascii=False 的 JSON 字符串。

use sea_orm::{DatabaseConnection, DbErr, EntityTrait, Set};
use serde_json::Value;

use crate::entities::audit_log;

/// 事务版：批量操作把审计行与业务变更同一事务提交
pub async fn log_action_tx<C: sea_orm::ConnectionTrait>(
    conn: &C,
    actor_type: &str,
    actor_id: Option<i32>,
    action: &str,
    target: Option<&str>,
    detail: Option<Value>,
) -> Result<(), sea_orm::DbErr> {
    let backend = conn.get_database_backend();
    conn.execute_raw(sea_orm::Statement::from_sql_and_values(
        backend,
        "INSERT INTO audit_logs (actor_type, actor_id, action, target, detail, created_at)          VALUES ($1, $2, $3, $4, $5, $6)",
        [
            actor_type.into(),
            actor_id.into(),
            action.into(),
            target.into(),
            detail
                .map(|d| serde_json::to_string(&d).unwrap_or_default())
                .into(),
            chrono::Utc::now().timestamp().into(),
        ],
    ))
    .await?;
    Ok(())
}

pub async fn log_action(
    db: &DatabaseConnection,
    actor_type: &str,
    actor_id: Option<i32>,
    action: &str,
    target: Option<&str>,
    detail: Option<Value>,
) -> Result<(), DbErr> {
    let detail_text = detail.map(|d| d.to_string());
    let record = audit_log::ActiveModel {
        actor_type: Set(actor_type.to_string()),
        actor_id: Set(actor_id),
        action: Set(action.to_string()),
        target: Set(target.map(|s| s.to_string())),
        detail: Set(detail_text),
        created_at: Set(chrono::Utc::now().timestamp()),
        ..Default::default()
    };
    audit_log::Entity::insert(record).exec(db).await?;
    Ok(())
}

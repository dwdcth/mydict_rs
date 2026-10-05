//! 审计日志 —— 移植自 `app/services/audit_service.py`。
//! 仅记录 admin/system 操作；detail 是 ensure_ascii=False 的 JSON 字符串。

use sea_orm::{DatabaseConnection, DbErr, EntityTrait, Set};
use serde_json::Value;

use crate::entities::audit_log;

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

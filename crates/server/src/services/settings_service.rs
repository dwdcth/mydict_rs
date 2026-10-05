//! 系统设置 KV 读写 —— 移植自 `app/services/settings_service.py`。

use sea_orm::{DatabaseConnection, DbErr, EntityTrait, Set};
use serde_json::Value;

use crate::entities::system_setting;

pub async fn get_setting(
    db: &DatabaseConnection,
    key: &str,
    default: Option<&str>,
) -> Result<Option<String>, DbErr> {
    let row = system_setting::Entity::find_by_id(key.to_string())
        .one(db)
        .await?;
    match row {
        Some(row) => Ok(row.value),
        None => Ok(default.map(|s| s.to_string())),
    }
}

pub async fn get_bool_setting(
    db: &DatabaseConnection,
    key: &str,
    default: bool,
) -> Result<bool, DbErr> {
    match get_setting(db, key, None).await? {
        Some(value) => Ok(value.to_lowercase().as_str() == "true"
            || value.to_lowercase().as_str() == "1"
            || value.to_lowercase().as_str() == "yes"),
        None => Ok(default),
    }
}

pub async fn get_int_setting(
    db: &DatabaseConnection,
    key: &str,
    default: i64,
) -> Result<i64, DbErr> {
    match get_setting(db, key, None).await? {
        Some(value) => {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                return Ok(default);
            }
            Ok(trimmed.parse().unwrap_or(default))
        }
        None => Ok(default),
    }
}

pub async fn set_setting(
    db: &DatabaseConnection,
    key: &str,
    value: &str,
) -> Result<(), DbErr> {
    let now = chrono::Utc::now().timestamp();
    let existing = system_setting::Entity::find_by_id(key.to_string())
        .one(db)
        .await?;
    match existing {
        Some(_) => {
            let mut active: system_setting::ActiveModel = Default::default();
            active.key = Set(key.to_string());
            active.value = Set(Some(value.to_string()));
            active.updated_at = Set(now);
            system_setting::Entity::update(active)
                .exec(db)
                .await?;
        }
        None => {
            let active = system_setting::ActiveModel {
                key: Set(key.to_string()),
                value: Set(Some(value.to_string())),
                updated_at: Set(now),
            };
            system_setting::Entity::insert(active).exec(db).await?;
        }
    }
    Ok(())
}

pub async fn delete_setting(db: &DatabaseConnection, key: &str) -> Result<(), DbErr> {
    system_setting::Entity::delete_by_id(key.to_string())
        .exec(db)
        .await?;
    Ok(())
}

/// tests 用：从 JSON 对象取字符串字段（None/非字符串 → None）
#[allow(dead_code)]
pub(crate) fn json_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

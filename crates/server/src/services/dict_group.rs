//! 词典组（GoldenDict 式）：Web 用户专属的命名查询范围预设。
//!
//! 一个组 = 一份有序的词典 id 列表。查询本身不需要新参数——前端选组即把组员
//! 填进现有的 `dict=a,b,c` 勾选通道，组只负责「存」与「一键切换」。
//! 组内顺序保留（前端展示与导出按 position），查询结果排序仍按词典全局顺序
//! （与手动勾选一致的行为）。

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde_json::{json, Value};

use crate::core::errors::AppError;

/// 每用户组数上限（防刷）
pub const MAX_GROUPS_PER_USER: i64 = 50;

fn validate_name(name: &str) -> Result<(), AppError> {
    let len = name.trim().chars().count();
    if len < 1 || len > 50 {
        return Err(AppError::validation("词典组名称长度须为 1-50 个字符"));
    }
    Ok(())
}

/// 组员去重（保序）：按首次出现保留
fn dedup_ids(ids: &[i32]) -> Vec<i32> {
    let mut seen = std::collections::HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// 列出用户全部组（含成员 id，按组内顺序）
pub async fn list_groups(db: &DatabaseConnection, user_id: i32) -> Result<Vec<Value>, AppError> {
    let backend = db.get_database_backend();
    let groups = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT id, name, created_at FROM dictionary_groups WHERE user_id = $1 ORDER BY created_at, id",
            [user_id.into()],
        ))
        .await?;
    let mut out = Vec::with_capacity(groups.len());
    for group in &groups {
        let group_id: i32 = group.try_get("", "id").unwrap_or_default();
        let items = db
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT dictionary_id FROM dictionary_group_items \
                 WHERE group_id = $1 ORDER BY position, dictionary_id",
                [group_id.into()],
            ))
            .await?;
        out.push(json!({
            "id": group_id,
            "name": group.try_get::<String>("", "name").unwrap_or_default(),
            "dictionary_ids": items.iter()
                .filter_map(|r| r.try_get::<i32>("", "dictionary_id").ok())
                .collect::<Vec<_>>(),
            "created_at": group.try_get::<i64>("", "created_at").unwrap_or_default(),
        }));
    }
    Ok(out)
}

/// 建组：name 唯一（同用户）；成员必须全部存在（不存在 → 422 指名道姓）
pub async fn create_group(
    db: &DatabaseConnection,
    user_id: i32,
    name: &str,
    dictionary_ids: &[i32],
) -> Result<Value, AppError> {
    validate_name(name)?;
    let ids = dedup_ids(dictionary_ids);
    if ids.is_empty() {
        return Err(AppError::validation("词典组至少包含一部词典"));
    }
    let backend = db.get_database_backend();
    let count = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM dictionary_groups WHERE user_id = $1",
            [user_id.into()],
        ))
        .await?
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    if count >= MAX_GROUPS_PER_USER as i64 {
        return Err(AppError::validation(format!(
            "词典组最多 {MAX_GROUPS_PER_USER} 个"
        )));
    }
    ensure_dicts_exist(db, &ids).await?;

    let tx = db.begin().await.map_err(AppError::from)?;
    let inserted = tx
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO dictionary_groups (user_id, name, created_at) \
             VALUES ($1, $2, $3) RETURNING id",
            [
                user_id.into(),
                name.trim().into(),
                chrono::Utc::now().timestamp().into(),
            ],
        ))
        .await;
    let group_id: i32 = match inserted {
        Ok(Some(row)) => row.try_get("", "id").unwrap_or_default(),
        // 同用户重名撞唯一约束
        Err(e) if e.to_string().contains("UNIQUE") => {
            return Err(AppError::conflict("同名词典组已存在"));
        }
        Err(e) => return Err(AppError::from(e)),
        Ok(None) => return Err(AppError::internal("group-insert", "no returning row")),
    };
    insert_items(&tx, group_id, &ids).await?;
    tx.commit().await.map_err(AppError::from)?;
    Ok(json!({
        "id": group_id,
        "name": name.trim(),
        "dictionary_ids": ids,
        "created_at": chrono::Utc::now().timestamp(),
    }))
}

/// 改组：改名与成员全量替换在同一事务；归属校验（别人的组按不存在处理）
pub async fn update_group(
    db: &DatabaseConnection,
    user_id: i32,
    group_id: i32,
    name: Option<&str>,
    dictionary_ids: Option<&[i32]>,
) -> Result<Value, AppError> {
    let backend = db.get_database_backend();
    let owned = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT id FROM dictionary_groups WHERE id = $1 AND user_id = $2",
            [group_id.into(), user_id.into()],
        ))
        .await?
        .is_some();
    if !owned {
        return Err(AppError::not_found("词典组不存在"));
    }
    if let Some(name) = name {
        validate_name(name)?;
    }
    let ids = match dictionary_ids {
        Some(raw) => {
            let ids = dedup_ids(raw);
            if ids.is_empty() {
                return Err(AppError::validation("词典组至少包含一部词典"));
            }
            ensure_dicts_exist(db, &ids).await?;
            Some(ids)
        }
        None => None,
    };

    let tx = db.begin().await.map_err(AppError::from)?;
    if let Some(name) = name {
        let updated = tx
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE dictionary_groups SET name = $1 WHERE id = $2 AND user_id = $3",
                [name.trim().into(), group_id.into(), user_id.into()],
            ))
            .await?;
        if updated.rows_affected() == 0 {
            return Err(AppError::not_found("词典组不存在"));
        }
    }
    if let Some(ids) = &ids {
        tx.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM dictionary_group_items WHERE group_id = $1",
            [group_id.into()],
        ))
        .await?;
        insert_items(&tx, group_id, ids).await?;
    }
    tx.commit().await.map_err(AppError::from)?;

    // 回读（成员可能只改了一半字段）
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT name, created_at FROM dictionary_groups WHERE id = $1",
            [group_id.into()],
        ))
        .await?
        .ok_or_else(|| AppError::not_found("词典组不存在"))?;
    let items = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT dictionary_id FROM dictionary_group_items \
             WHERE group_id = $1 ORDER BY position, dictionary_id",
            [group_id.into()],
        ))
        .await?;
    Ok(json!({
        "id": group_id,
        "name": row.try_get::<String>("", "name").unwrap_or_default(),
        "dictionary_ids": items.iter()
            .filter_map(|r| r.try_get::<i32>("", "dictionary_id").ok())
            .collect::<Vec<_>>(),
        "created_at": row.try_get::<i64>("", "created_at").unwrap_or_default(),
    }))
}

/// 删组（级联清成员）
pub async fn delete_group(
    db: &DatabaseConnection,
    user_id: i32,
    group_id: i32,
) -> Result<(), AppError> {
    let backend = db.get_database_backend();
    let deleted = db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM dictionary_groups WHERE id = $1 AND user_id = $2",
            [group_id.into(), user_id.into()],
        ))
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::not_found("词典组不存在"));
    }
    Ok(())
}

async fn insert_items<C: ConnectionTrait>(
    conn: &C,
    group_id: i32,
    ids: &[i32],
) -> Result<(), AppError> {
    let backend = conn.get_database_backend();
    for (position, id) in ids.iter().enumerate() {
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO dictionary_group_items (group_id, dictionary_id, position) \
             VALUES ($1, $2, $3)",
            [group_id.into(), (*id).into(), (position as i32).into()],
        ))
        .await?;
    }
    Ok(())
}

async fn ensure_dicts_exist(
    db: &DatabaseConnection,
    ids: &[i32],
) -> Result<(), AppError> {
    let backend = db.get_database_backend();
    let placeholders = (1..=ids.len())
        .map(|i| match backend {
            sea_orm::DbBackend::Postgres => format!("${i}"),
            _ => "?".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    let values: Vec<sea_orm::Value> = ids.iter().map(|id| (*id).into()).collect();
    let found = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT COUNT(*) AS n FROM dictionaries WHERE id IN ({placeholders})"),
            values,
        ))
        .await?
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    if found != ids.len() as i64 {
        return Err(AppError::validation("词典组包含不存在的词典 ID"));
    }
    Ok(())
}

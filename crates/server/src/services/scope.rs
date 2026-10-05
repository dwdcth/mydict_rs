//! 关联表（可用词典授权）读写 —— 替代 Python 版的 JSON 列实现。
//!
//! 语义：无行 = 不限制（配合 users/api_tokens 上的 *_limited 布尔列）；
//! 空列表归一化为「不限制」（对齐 Python `filter_existing_dictionary_ids` 的
//! `return filtered or None`）。

use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};

async fn query_ids(
    db: &DatabaseConnection,
    sql: &str,
    owner_id: i32,
) -> Result<Vec<i32>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            sql,
            [owner_id.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .map(|row| row.try_get::<i32>("", "dictionary_id").unwrap_or_default())
        .collect())
}

/// 管理员划定的「可用词典」上限
pub async fn admin_granted_ids(db: &DatabaseConnection, user_id: i32) -> Result<Vec<i32>, DbErr> {
    query_ids(
        db,
        "SELECT dictionary_id FROM admin_dict_grants WHERE user_id = ? ORDER BY dictionary_id",
        user_id,
    )
    .await
}

/// 用户在上限内的自选
pub async fn self_granted_ids(db: &DatabaseConnection, user_id: i32) -> Result<Vec<i32>, DbErr> {
    query_ids(
        db,
        "SELECT dictionary_id FROM self_dict_grants WHERE user_id = ? ORDER BY dictionary_id",
        user_id,
    )
    .await
}

/// Token 的可用词典
pub async fn token_granted_ids(db: &DatabaseConnection, token_id: i32) -> Result<Vec<i32>, DbErr> {
    query_ids(
        db,
        "SELECT dictionary_id FROM token_dict_grants WHERE token_id = ? ORDER BY dictionary_id",
        token_id,
    )
    .await
}

async fn rewrite_grants(
    db: &DatabaseConnection,
    table: &str,
    owner_col: &str,
    owner_id: i32,
    ids: Option<&[i32]>,
) -> Result<(), DbErr> {
    let backend = db.get_database_backend();
    db.execute_raw(Statement::from_sql_and_values(
        backend,
        format!("DELETE FROM {table} WHERE {owner_col} = ?"),
        [owner_id.into()],
    ))
    .await?;
    if let Some(ids) = ids {
        for &dictionary_id in ids {
            db.execute_raw(Statement::from_sql_and_values(
                backend,
                format!("INSERT INTO {table} ({owner_col}, dictionary_id) VALUES (?, ?)"),
                [owner_id.into(), dictionary_id.into()],
            ))
            .await?;
        }
    }
    Ok(())
}

/// 覆写授权集合：ids=None → 清空并解除限制；Some 空切片 = 归一为解除限制；
/// Some(ids) = 全量替换（调用方负责过滤已删词典 id）。
pub async fn set_admin_grants(
    db: &DatabaseConnection,
    user_id: i32,
    ids: Option<&[i32]>,
) -> Result<(), DbErr> {
    rewrite_grants(db, "admin_dict_grants", "user_id", user_id, ids).await
}

pub async fn set_self_grants(
    db: &DatabaseConnection,
    user_id: i32,
    ids: Option<&[i32]>,
) -> Result<(), DbErr> {
    rewrite_grants(db, "self_dict_grants", "user_id", user_id, ids).await
}

pub async fn set_token_grants(
    db: &DatabaseConnection,
    token_id: i32,
    ids: Option<&[i32]>,
) -> Result<(), DbErr> {
    rewrite_grants(db, "token_dict_grants", "token_id", token_id, ids).await
}

/// 过滤掉不存在的词典 id；结果为空视作 None（不限制）
pub async fn filter_existing_dictionary_ids(
    db: &DatabaseConnection,
    ids: &[i32],
) -> Result<Option<Vec<i32>>, DbErr> {
    if ids.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let exists = db
            .query_one_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT id FROM dictionaries WHERE id = ?",
                [id.into()],
            ))
            .await?
            .is_some();
        if exists {
            out.push(id);
        }
    }
    Ok((!out.is_empty()).then_some(out))
}

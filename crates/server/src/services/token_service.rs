//! API Token 管理 —— 移植自 `app/services/token_service.py`。
//!
//! Rust 版差异：用户 Token 的明文改为 AES-256-GCM 密文存储（token_secret BLOB，
//! key = SHA-256(jwt_secret.key 文件内容)），管理端按需解密展示——功能不变，
//! DB 文件单独泄露时不可直接复用。普通 Token 仍只存哈希。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::core::errors::AppError;
use crate::core::security::{generate_api_token, hash_api_token, token_display_prefix};
use crate::core::timeutil;
use crate::services::{audit_service, scope};
use crate::AppState;

fn encryption_key(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.finalize().into()
}

/// AES-256-GCM 加密（输出 nonce[12] ‖ ciphertext+tag）
pub fn seal_token(plain: &str, secret: &str) -> Vec<u8> {
    let key = encryption_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("32 bytes key");
    let nonce_bytes = {
        use rand::RngCore;
        let mut buf = [0u8; 12];
        rand::rng().fill_bytes(&mut buf);
        buf
    };
    let sealed = cipher
        .encrypt(
            Nonce::from(&nonce_bytes),
            Payload {
                msg: plain.as_bytes(),
                aad: b"mydict-token",
            },
        )
        .expect("encrypt");
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&sealed);
    out
}

/// 解密失败（密钥轮换/数据损坏）返回 None，管理端显示为空
pub fn unseal_token(secret_data: &[u8], secret: &str) -> Option<String> {
    if secret_data.len() < 12 {
        return None;
    }
    let key = encryption_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let (nonce_bytes, ct) = secret_data.split_at(12);
    let plain = cipher
        .decrypt(
            Nonce::from(nonce_bytes),
            Payload {
                msg: ct,
                aad: b"mydict-token",
            },
        )
        .ok()?;
    String::from_utf8(plain).ok()
}

/// 读取 JWT secret（与 security.rs 同一来源：env 或 jwt_secret.key 文件）
fn jwt_secret_for_crypto(state: &AppState) -> String {
    if !state.cfg.jwt_secret.is_empty() {
        return state.cfg.jwt_secret.clone();
    }
    let path = std::path::Path::new(&state.cfg.config_storage_path).join("jwt_secret.key");
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

async fn get_or_404(
    db: &DatabaseConnection,
    token_id: i32,
) -> Result<crate::entities::api_token::Model, AppError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    crate::entities::api_token::Entity::find()
        .filter(crate::entities::api_token::Column::Id.eq(token_id))
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("Token 不存在"))
}

async fn usage_maps(
    db: &DatabaseConnection,
) -> Result<(std::collections::HashMap<i32, i64>, std::collections::HashMap<i32, i64>), AppError> {
    let zone = timeutil::local_zone("");
    let today = timeutil::today_str(zone);
    let mut today_map = std::collections::HashMap::new();
    let mut total_map = std::collections::HashMap::new();
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT stat_date, token_id, query_count FROM token_usage_daily",
            [],
        ))
        .await?;
    for row in rows {
        let stat_date: String = row.try_get("", "stat_date").unwrap_or_default();
        let token_id: i32 = row.try_get("", "token_id").unwrap_or_default();
        let count: i64 = row.try_get::<i32>("", "query_count").map(|c| c as i64).unwrap_or(0);
        if stat_date == today {
            today_map.insert(token_id, count);
        }
        *total_map.entry(token_id).or_insert(0) += count;
    }
    Ok((today_map, total_map))
}

fn token_to_json(
    token: &crate::entities::api_token::Model,
    today_count: i64,
    total_count: i64,
    username: Option<&str>,
    allowed_ids: Option<Vec<i32>>,
) -> Value {
    json!({
        "id": token.id,
        "name": token.name,
        "token_prefix": token.token_prefix,
        "daily_limit": token.daily_limit,
        "status": token.status,
        "created_at": timeutil::unix_to_iso(token.created_at),
        "last_used_at": token.last_used_at.map(timeutil::unix_to_iso),
        "today_count": today_count,
        "total_count": total_count,
        "allowed_dictionary_ids": allowed_ids,
        "user_id": token.user_id,
        "username": username,
    })
}

async fn token_allowed_ids(
    db: &DatabaseConnection,
    token: &crate::entities::api_token::Model,
) -> Option<Vec<i32>> {
    if token.scope_limited != 0 {
        scope::token_granted_ids(db, token.id).await.ok()
    } else {
        None
    }
}

async fn username_of(
    db: &DatabaseConnection,
    user_id: Option<i32>,
) -> Result<Option<String>, AppError> {
    match user_id {
        Some(user_id) => {
            use sea_orm::EntityTrait;
            Ok(crate::entities::user::Entity::find_by_id(user_id)
                .one(db)
                .await?
                .map(|u| u.username))
        }
        None => Ok(None),
    }
}

pub async fn list_tokens(state: &AppState) -> Result<Vec<Value>, AppError> {
    let tokens = state
        .db
        .query_all_raw(Statement::from_string(
            state.db.get_database_backend(),
            "SELECT * FROM api_tokens ORDER BY created_at DESC, id DESC".to_string(),
        ))
        .await?;
    let (today_map, total_map) = usage_maps(&state.db).await?;
    let mut out = Vec::new();
    for row in &tokens {
        let token = row_to_token(row);
        let username = username_of(&state.db, token.user_id).await?;
        let allowed = token_allowed_ids(&state.db, &token).await;
        out.push(token_to_json(
            &token,
            today_map.get(&token.id).copied().unwrap_or(0),
            total_map.get(&token.id).copied().unwrap_or(0),
            username.as_deref(),
            allowed,
        ));
    }
    Ok(out)
}

fn row_to_token(row: &sea_orm::QueryResult) -> crate::entities::api_token::Model {
    crate::entities::api_token::Model {
        id: row.try_get("", "id").unwrap_or_default(),
        name: row.try_get("", "name").unwrap_or_default(),
        token_hash: row.try_get("", "token_hash").unwrap_or_default(),
        token_prefix: row.try_get("", "token_prefix").unwrap_or_default(),
        daily_limit: row.try_get::<Option<i32>>("", "daily_limit").ok().flatten(),
        status: row.try_get("", "status").unwrap_or_default(),
        created_at: row.try_get("", "created_at").unwrap_or_default(),
        created_by: row.try_get::<Option<i32>>("", "created_by").ok().flatten(),
        last_used_at: row.try_get::<Option<i64>>("", "last_used_at").ok().flatten(),
        user_id: row.try_get::<Option<i32>>("", "user_id").ok().flatten(),
        scope_limited: row.try_get("", "scope_limited").unwrap_or_default(),
        token_secret: row
            .try_get::<Option<Vec<u8>>>("", "token_secret")
            .ok()
            .flatten(),
    }
}

pub async fn get_token_out(state: &AppState, token_id: i32) -> Result<Value, AppError> {
    let token = get_or_404(&state.db, token_id).await?;
    let (today_map, total_map) = usage_maps(&state.db).await?;
    let username = username_of(&state.db, token.user_id).await?;
    let allowed = token_allowed_ids(&state.db, &token).await;
    Ok(token_to_json(
        &token,
        today_map.get(&token.id).copied().unwrap_or(0),
        total_map.get(&token.id).copied().unwrap_or(0),
        username.as_deref(),
        allowed,
    ))
}

/// 换新密钥：更新 hash/prefix；用户 Token 存密文（管理端可再取），普通 Token 不存明文
async fn assign_secret(
    state: &AppState,
    token_id: i32,
    is_user_token: bool,
) -> Result<String, AppError> {
    let raw = generate_api_token();
    let token_hash = hash_api_token(&raw);
    let token_prefix = token_display_prefix(&raw);
    let secret_blob: Option<Vec<u8>> = if is_user_token {
        Some(seal_token(&raw, &jwt_secret_for_crypto(state)))
    } else {
        None
    };
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE api_tokens SET token_hash = $1, token_prefix = $2, token_secret = $3 WHERE id = $4",
            [
                token_hash.into(),
                token_prefix.into(),
                secret_blob.into(),
                token_id.into(),
            ],
        ))
        .await?;
    Ok(raw)
}

pub async fn create_token(
    state: &AppState,
    name: &str,
    daily_limit: Option<i64>,
    admin_id: i32,
    allowed_dictionary_ids: Option<Vec<i32>>,
) -> Result<Value, AppError> {
    let raw = generate_api_token();
    let allowed = match allowed_dictionary_ids {
        Some(ids) => scope::filter_existing_dictionary_ids(&state.db, &ids).await?,
        None => None,
    };
    let scope_limited = allowed.is_some() as i32;
    let now = chrono::Utc::now().timestamp();
    let result = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "INSERT INTO api_tokens (name, token_hash, token_prefix, daily_limit, status, created_at, created_by, scope_limited) \
             VALUES ($1, $2, $3, $4, 'active', $5, $6, $7) RETURNING id",
            [
                name.into(),
                hash_api_token(&raw).into(),
                token_display_prefix(&raw).into(),
                daily_limit.into(),
                now.into(),
                admin_id.into(),
                scope_limited.into(),
            ],
        ))
        .await?;
    let token_id: i32 = result
        .and_then(|r| r.try_get("", "id").ok())
        .unwrap_or_default();
    if let Some(allowed) = &allowed {
        scope::set_token_grants(&state.db, token_id, Some(allowed)).await?;
    }
    audit_service::log_action(&state.db, "admin", Some(admin_id), "token.create", Some(&token_id.to_string()), None).await?;
    let mut out = get_token_out(state, token_id).await?;
    out["token"] = json!(raw);
    Ok(out)
}

pub async fn set_token_status(
    state: &AppState,
    token_id: i32,
    status: &str,
    admin_id: i32,
) -> Result<Value, AppError> {
    get_or_404(&state.db, token_id).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE api_tokens SET status = $1 WHERE id = $2",
            [status.into(), token_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        &format!("token.{status}"),
        Some(&token_id.to_string()),
        None,
    )
    .await?;
    get_token_out(state, token_id).await
}

pub async fn set_token_allowed_dictionaries(
    state: &AppState,
    token_id: i32,
    dictionary_ids: Option<Vec<i32>>,
    admin_id: i32,
) -> Result<Value, AppError> {
    let token = get_or_404(&state.db, token_id).await?;
    if token.user_id.is_some() {
        return Err(AppError::conflict(
            "用户 Token 的可用词典跟随所属用户，请在用户管理里设置",
        ));
    }
    let filtered = match dictionary_ids {
        Some(ids) => scope::filter_existing_dictionary_ids(&state.db, &ids).await?,
        None => None,
    };
    scope::set_token_grants(&state.db, token_id, filtered.as_deref()).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE api_tokens SET scope_limited = $1 WHERE id = $2",
            [(filtered.is_some() as i32).into(), token_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "token.set_allowed_dictionaries",
        Some(&token_id.to_string()),
        Some(json!({"dictionary_ids": filtered})),
    )
    .await?;
    get_token_out(state, token_id).await
}

pub async fn regenerate_token(state: &AppState, token_id: i32, admin_id: i32) -> Result<Value, AppError> {
    let token = get_or_404(&state.db, token_id).await?;
    let raw = assign_secret(state, token_id, token.user_id.is_some()).await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "token.regenerate",
        Some(&token_id.to_string()),
        None,
    )
    .await?;
    let mut out = get_token_out(state, token_id).await?;
    out["token"] = json!(raw);
    Ok(out)
}

/// 为用户签发 Token；已有则换新密钥（旧值立即失效）。每日上限用系统默认。
/// admin_id=None 表示用户前台自助（不写审计）。
pub async fn issue_user_token(
    state: &AppState,
    user: &crate::entities::user::Model,
    admin_id: Option<i32>,
) -> Result<i32, AppError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let existing = crate::entities::api_token::Entity::find()
        .filter(crate::entities::api_token::Column::UserId.eq(user.id))
        .one(&state.db)
        .await?;
    let (token_id, action) = match existing {
        Some(token) => (token.id, "token.regenerate"),
        None => {
            let name = format!("用户 {}", user.username);
            let now = chrono::Utc::now().timestamp();
            // token_hash NOT NULL：插入时给占位值，紧随其后的 assign_secret 会立即覆盖
            let result = state
                .db
                .query_one_raw(Statement::from_sql_and_values(
                    state.db.get_database_backend(),
                    "INSERT INTO api_tokens (name, token_hash, token_prefix, status, created_at, created_by, user_id) \
                     VALUES ($1, 'pending', 'pending', 'active', $2, $3, $4) RETURNING id",
                    [name.into(), now.into(), admin_id.into(), user.id.into()],
                ))
                .await?;
            let id: i32 = result
                .and_then(|r| r.try_get("", "id").ok())
                .unwrap_or_default();
            (id, "token.create")
        }
    };
    assign_secret(state, token_id, true).await?;
    if let Some(admin_id) = admin_id {
        audit_service::log_action(
            &state.db,
            "admin",
            Some(admin_id),
            action,
            Some(&token_id.to_string()),
            Some(json!({"user_id": user.id})),
        )
        .await?;
    }
    Ok(token_id)
}

/// 用户 Token 明文（解密）；给管理后台/前台「复制」用
pub async fn user_token_plain(state: &AppState, user_id: i32) -> Result<Option<String>, AppError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let token = crate::entities::api_token::Entity::find()
        .filter(crate::entities::api_token::Column::UserId.eq(user_id))
        .one(&state.db)
        .await?;
    Ok(token
        .and_then(|t| t.token_secret)
        .and_then(|blob| unseal_token(&blob, &jwt_secret_for_crypto(state))))
}

pub async fn delete_token(state: &AppState, token_id: i32, admin_id: i32) -> Result<(), AppError> {
    get_or_404(&state.db, token_id).await?;
    // 历史数据保留但匿名化（query_logs/token_usage_daily 的 token_id 置 NULL）
    for table in ["query_logs", "token_usage_daily"] {
        state
            .db
            .execute_raw(Statement::from_sql_and_values(
                state.db.get_database_backend(),
                &format!("UPDATE {table} SET token_id = NULL WHERE token_id = $1"),
                [token_id.into()],
            ))
            .await?;
    }
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "DELETE FROM api_tokens WHERE id = $1",
            [token_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "token.delete",
        Some(&token_id.to_string()),
        None,
    )
    .await?;
    Ok(())
}

pub async fn get_vocab_count(state: &AppState, token_id: i32) -> Result<i64, AppError> {
    get_or_404(&state.db, token_id).await?;
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT COUNT(*) AS n FROM token_vocab_items WHERE token_id = $1",
            [token_id.into()],
        ))
        .await?;
    Ok(row
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0))
}

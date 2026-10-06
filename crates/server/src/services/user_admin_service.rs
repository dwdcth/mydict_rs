//! 用户管理（管理端）—— 移植自 `app/services/user_admin_service.py`。

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::core::security::{generate_temp_password, hash_password};
use crate::core::timeutil::{self, unix_to_iso};
use crate::services::{audit_service, scope as grants, token_service};
use crate::AppState;

pub struct UserRow {
    pub id: i32,
    pub username: String,
    pub email: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    pub admin_scope_limited: i32,
    pub self_scope_limited: i32,
}

fn row_to_user(row: &sea_orm::QueryResult) -> UserRow {
    UserRow {
        id: row.try_get("", "id").unwrap_or_default(),
        username: row.try_get("", "username").unwrap_or_default(),
        email: row.try_get::<Option<String>>("", "email").ok().flatten(),
        status: row.try_get("", "status").unwrap_or_default(),
        created_at: row.try_get("", "created_at").unwrap_or_default(),
        last_login_at: row.try_get::<Option<i64>>("", "last_login_at").ok().flatten(),
        admin_scope_limited: row.try_get("", "admin_scope_limited").unwrap_or_default(),
        self_scope_limited: row.try_get("", "self_scope_limited").unwrap_or_default(),
    }
}

fn user_to_json(
    u: &UserRow,
    vocab_count: i64,
    query_count: i64,
    allowed_ids: Option<Vec<i32>>,
    api_token: Option<&str>,
) -> Value {
    json!({
        "id": u.id,
        "username": u.username,
        "email": u.email,
        "status": u.status,
        "created_at": unix_to_iso(u.created_at),
        "last_login_at": u.last_login_at.map(unix_to_iso),
        "vocab_count": vocab_count,
        "query_count": query_count,
        "allowed_dictionary_ids": allowed_ids,
        "api_token": api_token,
    })
}

async fn counts_for(
    db: &DatabaseConnection,
    user_ids: &[i32],
) -> Result<(std::collections::HashMap<i32, i64>, std::collections::HashMap<i32, i64>), AppError> {
    if user_ids.is_empty() {
        return Ok((Default::default(), Default::default()));
    }
    let backend = db.get_database_backend();
    let ids: Vec<sea_orm::Value> = user_ids.iter().map(|id| (*id).into()).collect();
    let ph = crate::services::query::sql_placeholders(backend, ids.len());
    let mut vocab_map = std::collections::HashMap::new();
    let mut query_map = std::collections::HashMap::new();
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT user_id, COUNT(*) AS n FROM vocab_items WHERE user_id IN ({ph}) GROUP BY user_id"),
            ids.clone(),
        ))
        .await?;
    for row in rows {
        vocab_map.insert(
            row.try_get("", "user_id").unwrap_or_default(),
            row.try_get("", "n").unwrap_or_default(),
        );
    }
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT user_id, COUNT(*) AS n FROM query_logs WHERE user_id IN ({ph}) GROUP BY user_id"),
            ids,
        ))
        .await?;
    for row in rows {
        query_map.insert(
            row.try_get("", "user_id").unwrap_or_default(),
            row.try_get("", "n").unwrap_or_default(),
        );
    }
    Ok((vocab_map, query_map))
}

async fn admin_grants_for(
    db: &DatabaseConnection,
    u: &UserRow,
) -> Result<Option<Vec<i32>>, AppError> {
    if u.admin_scope_limited != 0 {
        Ok(Some(grants::admin_granted_ids(db, u.id).await?))
    } else {
        Ok(None)
    }
}

pub async fn list_users(
    state: &AppState,
    search: Option<&str>,
    status: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<(Vec<Value>, i64), AppError> {
    let backend = state.db.get_database_backend();
    let is_pg = backend == sea_orm::DbBackend::Postgres;
    // 占位符序号发生器：PG 用 $N（可复用），SQLite/MySQL 用 ?（逐个绑定）
    let mut next_ph = 1usize;
    let mut ph = || {
        let n = next_ph;
        next_ph += 1;
        if is_pg { format!("${n}") } else { "?".to_string() }
    };
    let mut where_clauses: Vec<String> = Vec::new();
    let mut params: Vec<sea_orm::Value> = Vec::new();
    if let Some(search) = search.filter(|s| !s.trim().is_empty()) {
        let like = format!("%{}%", search.trim());
        let p1 = ph();
        let p2 = ph();
        where_clauses.push(format!("(username LIKE {p1} OR email LIKE {p2})"));
        params.push(like.clone().into());
        params.push(like.into());
    }
    if let Some(status) = status.filter(|s| !s.is_empty()) {
        let p = ph();
        where_clauses.push(format!("status = {p}"));
        params.push(status.into());
    }
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };
    let total: i64 = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT COUNT(*) AS n FROM users {where_sql}"),
            params.clone(),
        ))
        .await?
        .and_then(|r| r.try_get("", "n").ok())
        .unwrap_or(0);
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT * FROM users {where_sql} ORDER BY created_at DESC, id DESC LIMIT {} OFFSET {}",
                page_size,
                (page - 1).max(0) * page_size
            ),
            params,
        ))
        .await?;
    let users: Vec<UserRow> = rows.iter().map(row_to_user).collect();
    let ids: Vec<i32> = users.iter().map(|u| u.id).collect();
    let (vocab_map, query_map) = counts_for(&state.db, &ids).await?;
    let mut out = Vec::with_capacity(users.len());
    for u in &users {
        let allowed = admin_grants_for(&state.db, u).await?;
        let api_token = token_service::user_token_plain(state, u.id).await?;
        out.push(user_to_json(
            u,
            vocab_map.get(&u.id).copied().unwrap_or(0),
            query_map.get(&u.id).copied().unwrap_or(0),
            allowed,
            api_token.as_deref(),
        ));
    }
    Ok((out, total))
}

async fn get_user_or_404(
    db: &DatabaseConnection,
    user_id: i32,
) -> Result<UserRow, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT * FROM users WHERE id = $1",
            [user_id.into()],
        ))
        .await?;
    row.as_ref()
        .map(row_to_user)
        .ok_or_else(|| AppError::not_found("用户不存在"))
}

pub async fn create_user(
    state: &AppState,
    username: &str,
    email: Option<&str>,
    admin_id: i32,
) -> Result<(Value, String), AppError> {
    let exists = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT id FROM users WHERE username = $1",
            [username.into()],
        ))
        .await?
        .is_some();
    if exists {
        return Err(AppError::conflict("用户名已存在"));
    }
    let temp_password = generate_temp_password();
    let password_hash = hash_password(&temp_password).map_err(|e| AppError::internal("bcrypt", e))?;
    let result = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "INSERT INTO users (username, email, password_hash, created_at) VALUES ($1, $2, $3, $4) RETURNING id",
            [
                username.into(),
                email.map(String::from).into(),
                password_hash.into(),
                chrono::Utc::now().timestamp().into(),
            ],
        ))
        .await?;
    let user_id: i32 = result
        .and_then(|r| r.try_get("", "id").ok())
        .unwrap_or_default();
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "user.create",
        Some(username),
        None,
    )
    .await?;
    let row = get_user_or_404(&state.db, user_id).await?;
    Ok((user_to_json(&row, 0, 0, None, None), temp_password))
}

async fn user_out(state: &AppState, u: &UserRow) -> Result<Value, AppError> {
    let (vocab_map, query_map) = counts_for(&state.db, &[u.id]).await?;
    let allowed = admin_grants_for(&state.db, u).await?;
    let api_token = token_service::user_token_plain(state, u.id).await?;
    Ok(user_to_json(
        u,
        vocab_map.get(&u.id).copied().unwrap_or(0),
        query_map.get(&u.id).copied().unwrap_or(0),
        allowed,
        api_token.as_deref(),
    ))
}

pub async fn set_user_status(
    state: &AppState,
    user_id: i32,
    status: &str,
    admin_id: i32,
) -> Result<Value, AppError> {
    get_user_or_404(&state.db, user_id).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE users SET status = $1 WHERE id = $2",
            [status.into(), user_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        &format!("user.{status}"),
        Some(&user_id.to_string()),
        None,
    )
    .await?;
    let row = get_user_or_404(&state.db, user_id).await?;
    user_out(state, &row).await
}

pub async fn reset_user_password(
    state: &AppState,
    user_id: i32,
    admin_id: i32,
) -> Result<String, AppError> {
    get_user_or_404(&state.db, user_id).await?;
    let temp_password = generate_temp_password();
    let password_hash = hash_password(&temp_password).map_err(|e| AppError::internal("bcrypt", e))?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE users SET password_hash = $1 WHERE id = $2",
            [password_hash.into(), user_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "user.reset_password",
        Some(&user_id.to_string()),
        None,
    )
    .await?;
    Ok(temp_password)
}

pub async fn get_user_detail(state: &AppState, user_id: i32) -> Result<Value, AppError> {
    let row = get_user_or_404(&state.db, user_id).await?;
    let user_json = user_out(state, &row).await?;
    let vocab_rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT v.id, v.word, v.dictionary_id, v.dictionary_name, v.phonetic, v.definition, v.note, v.created_at \
             FROM vocab_items v WHERE v.user_id = $1 ORDER BY v.created_at DESC, v.id DESC",
            [user_id.into()],
        ))
        .await?;
    let vocab_items: Vec<Value> = vocab_rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<i32>("", "id").unwrap_or_default(),
                "word": r.try_get::<String>("", "word").unwrap_or_default(),
                "dictionary_id": r.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
                "dictionary_name": r.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
                "phonetic": r.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                "definition": r.try_get::<Option<String>>("", "definition").ok().flatten(),
                "note": r.try_get::<Option<String>>("", "note").ok().flatten(),
                "created_at": unix_to_iso(r.try_get::<i64>("", "created_at").unwrap_or_default()),
            })
        })
        .collect();
    let recent_rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT word, status, dictionary_id, duration_ms, created_at FROM query_logs \
             WHERE user_id = $1 ORDER BY created_at DESC, id DESC LIMIT 20",
            [user_id.into()],
        ))
        .await?;
    let recent_queries: Vec<Value> = recent_rows
        .iter()
        .map(|r| {
            json!({
                "word": r.try_get::<String>("", "word").unwrap_or_default(),
                "status": r.try_get::<Option<String>>("", "status").ok().flatten(),
                "dictionary_id": r.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
                "duration_ms": r.try_get::<Option<i32>>("", "duration_ms").ok().flatten(),
                "created_at": unix_to_iso(r.try_get::<i64>("", "created_at").unwrap_or_default()),
            })
        })
        .collect();
    Ok(json!({
        "user": user_json,
        "vocab_items": vocab_items,
        "recent_queries": recent_queries,
    }))
}

pub async fn set_admin_allowed_dictionaries(
    state: &AppState,
    user_id: i32,
    dictionary_ids: Option<Vec<i32>>,
    admin_id: i32,
) -> Result<Value, AppError> {
    let _ = get_user_or_404(&state.db, user_id).await?;
    let filtered = match dictionary_ids {
        Some(ids) => grants::filter_existing_dictionary_ids(&state.db, &ids).await?,
        None => None,
    };
    grants::set_admin_grants(&state.db, user_id, filtered.as_deref()).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE users SET admin_scope_limited = $1 WHERE id = $2",
            [(filtered.is_some() as i32).into(), user_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "user.set_allowed_dictionaries",
        Some(&user_id.to_string()),
        Some(json!({"dictionary_ids": filtered})),
    )
    .await?;
    let row = get_user_or_404(&state.db, user_id).await?;
    user_out(state, &row).await
}

pub async fn generate_user_token(
    state: &AppState,
    user_id: i32,
    admin_id: i32,
) -> Result<Value, AppError> {
    let row = get_user_or_404(&state.db, user_id).await?;
    let user = crate::entities::user::Model {
        id: row.id,
        username: row.username.clone(),
        email: row.email.clone(),
        password_hash: String::new(),
        status: row.status.clone(),
        created_at: row.created_at,
        last_login_at: row.last_login_at,
        admin_scope_limited: row.admin_scope_limited,
        self_scope_limited: row.self_scope_limited,
        fsrs_retention: None,
        fsrs_weights: None,
    };
    token_service::issue_user_token(state, &user, Some(admin_id)).await?;
    user_out(state, &row).await
}

pub async fn delete_user_token(state: &AppState, user_id: i32, admin_id: i32) -> Result<Value, AppError> {
    let row = get_user_or_404(&state.db, user_id).await?;
    let token_row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT id FROM api_tokens WHERE user_id = $1",
            [user_id.into()],
        ))
        .await?;
    let Some(token_row) = token_row else {
        return Err(AppError::not_found("该用户没有 Token"));
    };
    let token_id: i32 = token_row.try_get("", "id").unwrap_or_default();
    token_service::delete_token(state, token_id, admin_id).await?;
    user_out(state, &row).await
}

pub async fn delete_user(state: &AppState, user_id: i32, admin_id: i32) -> Result<Value, AppError> {
    let row = get_user_or_404(&state.db, user_id).await?;
    let count = |sql: String| async move {
        state
            .db
            .query_one_raw(Statement::from_string(
                state.db.get_database_backend(),
                sql,
            ))
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<i64>("", "n").ok())
            .unwrap_or(0)
    };
    // 显式清理四张引用表（vocab/token_vocab CASCADE 也会兜底；这里按 Python 语义逐表计数）
    // 先计数再删（前端提示与审计留痕用）
    let vocab = count(format!("SELECT COUNT(*) AS n FROM vocab_items WHERE user_id = {user_id}")).await;
    let queries = count(format!("SELECT COUNT(*) AS n FROM query_logs WHERE user_id = {user_id}")).await;
    let stats = count(format!("SELECT COUNT(*) AS n FROM stats_daily WHERE user_id = {user_id}")).await;
    let tokens = count(format!("SELECT COUNT(*) AS n FROM api_tokens WHERE user_id = {user_id}")).await;
    for table in ["query_logs", "stats_daily", "api_tokens"] {
        state
            .db
            .execute_raw(Statement::from_string(
                state.db.get_database_backend(),
                format!("DELETE FROM {table} WHERE user_id = {user_id}"),
            ))
            .await?;
    }
    state
        .db
        .execute_raw(Statement::from_string(
            state.db.get_database_backend(),
            format!("DELETE FROM users WHERE id = {user_id}"),
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "user.delete",
        Some(&user_id.to_string()),
        Some(json!({
            "username": row.username,
            "vocab": vocab,
            "queries": queries,
            "stats": stats,
            "tokens": tokens,
        })),
    )
    .await?;
    Ok(json!({
        "username": row.username,
        "vocab": vocab,
        "queries": queries,
        "stats": stats,
        "tokens": tokens,
    }))
}

#[allow(dead_code)]
fn unused_zone() -> String {
    timeutil::today_str(crate::core::timeutil::local_zone(""))
}

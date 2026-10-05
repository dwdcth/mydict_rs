//! 用户认证 —— 移植自 `app/services/user_auth_service.py`。

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::config::Settings;
use crate::core::errors::AppError;
use crate::core::security::{
    hash_password, verify_password, AUD_USER,
};
use crate::entities::user;
use crate::services::{admin_auth_service, scope};
use crate::AppState;

pub fn user_public_json(u: &user::Model, allowed_ids: Option<Vec<i32>>) -> Value {
    json!({
        "id": u.id,
        "username": u.username,
        "email": u.email,
        "status": u.status,
        "allowed_dictionary_ids": allowed_ids,
    })
}

pub async fn register_user(
    state: &AppState,
    username: &str,
    password: &str,
    email: Option<&str>,
) -> Result<user::Model, AppError> {
    let allowed = crate::services::settings_service::get_bool_setting(
        &state.db,
        "allow_registration",
        state.cfg.allow_registration_default,
    )
    .await?;
    if !allowed {
        return Err(AppError::registration_disabled());
    }
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
    let password_hash = hash_password(password).map_err(|e| AppError::internal("bcrypt", e))?;
    let record = user::ActiveModel {
        username: sea_orm::Set(username.to_string()),
        email: sea_orm::Set(email.map(String::from)),
        password_hash: sea_orm::Set(password_hash),
        created_at: sea_orm::Set(chrono::Utc::now().timestamp()),
        ..Default::default()
    };
    use sea_orm::EntityTrait;
    Ok(user::Entity::insert(record)
        .exec_with_returning(&state.db)
        .await?)
}

pub async fn authenticate_user(
    settings: &Settings,
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<Value, AppError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let found = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(db)
        .await?;
    let user = match &found {
        Some(u) if verify_password(password, &u.password_hash) => u,
        _ => {
            tracing::warn!(username, "user login failed");
            return Err(AppError::invalid_credentials("用户名或密码错误"));
        }
    };
    if user.status != "active" {
        tracing::warn!(username, "user login rejected (disabled)");
        return Err(AppError::forbidden("账号已被禁用"));
    }
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE users SET last_login_at = $1 WHERE id = $2",
        [chrono::Utc::now().timestamp().into(), user.id.into()],
    ))
    .await?;
    tracing::info!(username = user.username, id = user.id, "user login ok");
    admin_auth_service::token_pair(settings, user.id, AUD_USER)
}

pub async fn change_password(
    state: &AppState,
    user: &user::Model,
    old_password: &str,
    new_password: &str,
) -> Result<(), AppError> {
    if !verify_password(old_password, &user.password_hash) {
        return Err(AppError::invalid_credentials("原密码不正确"));
    }
    let new_hash = hash_password(new_password).map_err(|e| AppError::internal("bcrypt", e))?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE users SET password_hash = $1 WHERE id = $2",
            [new_hash.into(), user.id.into()],
        ))
        .await?;
    Ok(())
}

/// 用户前台自选可用词典：夹在管理员上限内，过滤已删 id，空列表归一为 None
pub async fn set_self_allowed_dictionaries(
    state: &AppState,
    user: &user::Model,
    dictionary_ids: Option<Vec<i32>>,
) -> Result<user::Model, AppError> {
    let limit = if user.admin_scope_limited != 0 {
        Some(scope::admin_granted_ids(&state.db, user.id).await?)
    } else {
        None
    };
    let filtered = match dictionary_ids {
        Some(ids) => {
            let mut ids = ids;
            if let Some(limit) = &limit {
                ids.retain(|id| limit.contains(id));
            }
            scope::filter_existing_dictionary_ids(&state.db, &ids).await?
        }
        None => None,
    };
    scope::set_self_grants(&state.db, user.id, filtered.as_deref()).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE users SET self_scope_limited = $1 WHERE id = $2",
            [(filtered.is_some() as i32).into(), user.id.into()],
        ))
        .await?;
    use sea_orm::EntityTrait;
    Ok(user::Entity::find_by_id(user.id)
        .one(&state.db)
        .await?
        .expect("just updated"))
}

//! 管理员认证 —— 移植自 `app/services/admin_auth_service.py`。

use sea_orm::{ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set};
use serde_json::json;

use crate::core::config::Settings;
use crate::core::errors::AppError;
use crate::core::security::{
    create_access_token, create_refresh_token, hash_password, verify_password, AUD_ADMIN,
};
use crate::entities::admin;
use crate::services::audit_service;

pub async fn is_initialized(db: &DatabaseConnection) -> Result<bool, DbErr> {
    Ok(admin::Entity::find().one(db).await?.is_some())
}

pub async fn setup_admin(
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<admin::Model, AppError> {
    if is_initialized(db).await? {
        return Err(AppError::admin_already_initialized());
    }
    let password_hash = hash_password(password).map_err(|e| AppError::internal("bcrypt", e))?;
    let record = admin::ActiveModel {
        username: Set(username.to_string()),
        password_hash: Set(password_hash),
        created_at: Set(chrono::Utc::now().timestamp()),
        ..Default::default()
    };
    let inserted = admin::Entity::insert(record)
        .exec_with_returning(db)
        .await
        .map_err(AppError::from)?;
    audit_service::log_action(
        db,
        "admin",
        Some(inserted.id),
        "admin.setup",
        Some(username),
        None,
    )
    .await
    .map_err(AppError::from)?;
    Ok(inserted)
}

pub async fn authenticate_admin(
    settings: &Settings,
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<serde_json::Value, AppError> {
    let found = admin::Entity::find()
        .filter(admin::Column::Username.eq(username))
        .one(db)
        .await
        .map_err(AppError::from)?;
    let verified = found
        .as_ref()
        .is_some_and(|admin| verify_password(password, &admin.password_hash));
    if !verified {
        tracing::warn!(username, "admin login failed");
        return Err(AppError::invalid_credentials("用户名或密码错误"));
    }
    let admin = found.expect("verified above");
    let now = chrono::Utc::now().timestamp();
    let mut active: admin::ActiveModel = admin.clone().into();
    active.last_login_at = Set(Some(now));
    // Model → ActiveModel 转换后 id 为 Unchanged，update 按主键定位，无需 filter
    admin::Entity::update(active)
        .exec(db)
        .await
        .map_err(AppError::from)?;
    tracing::info!(username = admin.username, id = admin.id, "admin login ok");
    token_pair(settings, admin.id, AUD_ADMIN)
}

pub fn token_pair(settings: &Settings, subject: i32, aud: &str) -> Result<serde_json::Value, AppError> {
    Ok(json!({
        "access_token": create_access_token(settings, subject, aud)
            .map_err(|e| AppError::internal("jwt", e))?,
        "refresh_token": create_refresh_token(settings, subject, aud)
            .map_err(|e| AppError::internal("jwt", e))?,
        "token_type": "bearer",
    }))
}

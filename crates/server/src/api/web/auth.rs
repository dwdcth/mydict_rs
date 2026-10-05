//! /api/auth/* —— 移植自 `app/api/web/auth.py`（用户注册/登录/刷新/改密/自选词典/用户Token）。

use actix_web::web;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::UserAuth;
use crate::core::errors::AppError;
use crate::core::security::{create_access_token, decode_token, AUD_USER};
use crate::services::user_auth_service as auth;
use crate::AppState;

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct AllowedDictionariesRequest {
    pub dictionary_ids: Option<Vec<i32>>,
}

fn validate_credentials(username: &str, password: &str) -> Result<(), AppError> {
    let username_len = username.chars().count();
    if !(3..=64).contains(&username_len) {
        return Err(AppError::validation("用户名长度须为 3-64 个字符"));
    }
    let password_len = password.chars().count();
    if !(8..=128).contains(&password_len) {
        return Err(AppError::validation("密码长度须为 8-128 个字符"));
    }
    Ok(())
}

pub async fn register(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<RegisterRequest>,
) -> Result<web::Json<Value>, AppError> {
    validate_credentials(&body.username, &body.password)?;
    if let Some(email) = &body.email {
        if !email.contains('@') || email.len() > 255 {
            return Err(AppError::validation("邮箱格式无效"));
        }
    }
    let user = auth::register_user(&app, &body.username, &body.password, body.email.as_deref()).await?;
    let allowed = crate::core::deps::user_allowed_dictionary_ids(&app.db, &user).await;
    Ok(web::Json(auth::user_public_json(&user, allowed)))
}

pub async fn login(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<LoginRequest>,
) -> Result<web::Json<Value>, AppError> {
    Ok(web::Json(
        auth::authenticate_user(&app.cfg, &app.db, &body.username, &body.password).await?,
    ))
}

pub async fn refresh(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<RefreshRequest>,
) -> Result<web::Json<Value>, AppError> {
    let claims = decode_token(&app.cfg, &body.refresh_token, AUD_USER)
        .map_err(|_| AppError::unauthorized("刷新凭证无效或已过期"))?;
    if claims.scope != "refresh" {
        return Err(AppError::unauthorized("凭证类型不正确"));
    }
    let user_id: i32 = claims
        .sub
        .parse()
        .map_err(|_| AppError::unauthorized("刷新凭证无效或已过期"))?;
    Ok(web::Json(json!({
        "access_token": create_access_token(&app.cfg, user_id, AUD_USER)
            .map_err(|e| AppError::internal("jwt", e))?,
        "refresh_token": body.refresh_token,
        "token_type": "bearer",
    })))
}

pub async fn change_password(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
    body: web::Json<ChangePasswordRequest>,
) -> Result<web::Json<Value>, AppError> {
    let new_len = body.new_password.chars().count();
    if !(8..=128).contains(&new_len) {
        return Err(AppError::validation("密码长度须为 8-128 个字符"));
    }
    auth::change_password(&app, &user.0, &body.old_password, &body.new_password).await?;
    Ok(web::Json(json!({"ok": true})))
}

pub async fn me(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
) -> Result<web::Json<Value>, AppError> {
    let allowed = crate::core::deps::user_allowed_dictionary_ids(&app.db, &user.0).await;
    Ok(web::Json(auth::user_public_json(&user.0, allowed)))
}

pub async fn set_allowed_dictionaries(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
    body: web::Json<AllowedDictionariesRequest>,
) -> Result<web::Json<Value>, AppError> {
    let user = auth::set_self_allowed_dictionaries(&app, &user.0, body.dictionary_ids.clone()).await?;
    let allowed = crate::core::deps::user_allowed_dictionary_ids(&app.db, &user).await;
    Ok(web::Json(auth::user_public_json(&user, allowed)))
}

/// GET /api/auth/api-token —— 用户自助查看自己的 Token 明文
pub async fn get_api_token(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
) -> Result<web::Json<Value>, AppError> {
    let token = crate::services::token_service::user_token_plain(&app, user.0.id).await?;
    Ok(web::Json(json!({"api_token": token})))
}

/// POST /api/auth/api-token —— 自助签发（已有则换新，旧值立即失效）
pub async fn issue_api_token(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
) -> Result<web::Json<Value>, AppError> {
    crate::services::token_service::issue_user_token(&app, &user.0, None).await?;
    let token = crate::services::token_service::user_token_plain(&app, user.0.id).await?;
    Ok(web::Json(json!({"api_token": token})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/auth/register", web::post().to(register))
        .route("/auth/login", web::post().to(login))
        .route("/auth/refresh", web::post().to(refresh))
        .route("/auth/change-password", web::post().to(change_password))
        .route("/auth/me", web::get().to(me))
        .route("/auth/allowed-dictionaries", web::put().to(set_allowed_dictionaries))
        .route("/auth/api-token", web::get().to(get_api_token))
        .route("/auth/api-token", web::post().to(issue_api_token));
}

//! /api/admin/auth —— 移植自 `app/api/admin/auth.py`。

use actix_web::{web, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::core::errors::AppError;
use crate::core::security::{create_access_token, decode_token, AUD_ADMIN};
use crate::services::admin_auth_service as auth_service;
use crate::AppState;

#[derive(Deserialize)]
pub struct AdminSetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct AdminLoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
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

pub async fn bootstrap_status(app: web::Data<std::sync::Arc<AppState>>) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(json!({
        "initialized": auth_service::is_initialized(&app.db).await?,
    })))
}

pub async fn setup(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<AdminSetupRequest>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    validate_credentials(&body.username, &body.password)?;
    auth_service::setup_admin(&app.db, &body.username, &body.password).await?;
    let pair = auth_service::authenticate_admin(&app.cfg, &app.db, &body.username, &body.password).await?;
    Ok(web::Json(pair))
}

pub async fn login(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<AdminLoginRequest>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(
        auth_service::authenticate_admin(&app.cfg, &app.db, &body.username, &body.password).await?,
    ))
}

pub async fn refresh(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<RefreshRequest>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let claims = decode_token(&app.cfg, &body.refresh_token, AUD_ADMIN)
        .map_err(|_| AppError::unauthorized("刷新凭证无效或已过期"))?;
    if claims.scope != "refresh" {
        return Err(AppError::unauthorized("凭证类型不正确"));
    }
    let admin_id: i32 = claims
        .sub
        .parse()
        .map_err(|_| AppError::unauthorized("刷新凭证无效或已过期"))?;
    // 对齐 Python：只发新 access，refresh 原样返回、不轮换
    Ok(web::Json(json!({
        "access_token": create_access_token(&app.cfg, admin_id, AUD_ADMIN).map_err(|e| AppError::internal("jwt", e))?,
        "refresh_token": body.refresh_token,
        "token_type": "bearer",
    })))
}

pub async fn me(
    app: web::Data<std::sync::Arc<AppState>>,
    req: actix_web::HttpRequest,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let admin = crate::core::deps::require_admin(&req, &app).await?;
    Ok(web::Json(json!({"id": admin.id, "username": admin.username})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/bootstrap-status", web::get().to(bootstrap_status))
        .route("/admin/setup", web::post().to(setup))
        .route("/admin/login", web::post().to(login))
        .route("/admin/refresh", web::post().to(refresh))
        .route("/admin/me", web::get().to(me));
}

#[allow(unused)]
fn _marker(_: HttpResponse) {}

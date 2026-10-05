//! /api/admin/tokens —— 移植自 `app/api/admin/tokens.py`。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::require_admin;
use crate::core::errors::AppError;
use crate::services::token_service as svc;
use crate::AppState;

pub async fn list(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<Vec<Value>>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(svc::list_tokens(&app).await?))
}

#[derive(Deserialize)]
pub struct CreateRequest {
    pub name: String,
    pub daily_limit: Option<i64>,
    pub allowed_dictionary_ids: Option<Vec<i32>>,
}

pub async fn create(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<CreateRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let len = body.name.trim().chars().count();
    if !(1..=255).contains(&len) {
        return Err(AppError::validation("Token 名称长度须为 1-255 个字符"));
    }
    Ok(web::Json(
        svc::create_token(&app, body.name.trim(), body.daily_limit, admin.id, body.allowed_dictionary_ids.clone()).await?,
    ))
}

pub async fn enable(app: web::Data<std::sync::Arc<AppState>>, req: HttpRequest, path: web::Path<i32>) -> Result<web::Json<Value>, AppError> {
    set_status(app, req, path, "active").await
}

pub async fn disable(app: web::Data<std::sync::Arc<AppState>>, req: HttpRequest, path: web::Path<i32>) -> Result<web::Json<Value>, AppError> {
    set_status(app, req, path, "disabled").await
}

async fn set_status(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    status: &str,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(svc::set_token_status(&app, path.into_inner(), status, admin.id).await?))
}

pub async fn regenerate(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(svc::regenerate_token(&app, path.into_inner(), admin.id).await?))
}

#[derive(Deserialize)]
pub struct AllowedRequest {
    pub dictionary_ids: Option<Vec<i32>>,
}

pub async fn set_allowed_dictionaries(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    body: web::Json<AllowedRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(
        svc::set_token_allowed_dictionaries(&app, path.into_inner(), body.dictionary_ids.clone(), admin.id).await?,
    ))
}

/// 删除：204 无 body（对齐 Python）；日志匿名化在 service 内
pub async fn delete(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<HttpResponse, AppError> {
    let admin = require_admin(&req, &app).await?;
    svc::delete_token(&app, path.into_inner(), admin.id).await?;
    Ok(HttpResponse::NoContent().finish())
}

pub async fn vocab_count(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(json!({"count": svc::get_vocab_count(&app, path.into_inner()).await?})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/tokens", web::get().to(list))
        .route("/admin/tokens", web::post().to(create))
        .route("/admin/tokens/{id}/enable", web::put().to(enable))
        .route("/admin/tokens/{id}/disable", web::put().to(disable))
        .route("/admin/tokens/{id}/regenerate", web::post().to(regenerate))
        .route("/admin/tokens/{id}/allowed-dictionaries", web::put().to(set_allowed_dictionaries))
        .route("/admin/tokens/{id}", web::delete().to(delete))
        .route("/admin/tokens/{id}/vocab-count", web::get().to(vocab_count));
}

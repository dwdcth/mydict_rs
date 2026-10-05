//! /api/admin/users —— 移植自 `app/api/admin/users.py`。

use actix_web::{web, HttpRequest};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::require_admin;
use crate::core::errors::AppError;
use crate::services::user_admin_service as svc;
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 { 1 }
fn default_page_size() -> i64 { 20 }

pub async fn list(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    query: web::Query<ListQuery>,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    let page_size = query.page_size;
    let (items, total) = svc::list_users(
        &app,
        query.search.as_deref(),
        query.status.as_deref(),
        query.page.max(1),
        page_size.clamp(1, 100),
    )
    .await?;
    Ok(web::Json(json!({
        "items": items,
        "total": total,
        "page": query.page,
        "page_size": page_size,
    })))
}

#[derive(Deserialize)]
pub struct CreateRequest {
    pub username: String,
    pub email: Option<String>,
}

pub async fn create(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<CreateRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let len = body.username.trim().chars().count();
    if !(3..=64).contains(&len) {
        return Err(AppError::validation("用户名长度须为 3-64 个字符"));
    }
    let (user, temporary_password) =
        svc::create_user(&app, body.username.trim(), body.email.as_deref(), admin.id).await?;
    Ok(web::Json(json!({"user": user, "temporary_password": temporary_password})))
}

pub async fn detail(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(svc::get_user_detail(&app, path.into_inner()).await?))
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
    Ok(web::Json(svc::set_user_status(&app, path.into_inner(), status, admin.id).await?))
}

pub async fn reset_password(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let temporary_password = svc::reset_user_password(&app, path.into_inner(), admin.id).await?;
    Ok(web::Json(json!({"temporary_password": temporary_password})))
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
        svc::set_admin_allowed_dictionaries(&app, path.into_inner(), body.dictionary_ids.clone(), admin.id).await?,
    ))
}

pub async fn generate_token(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(svc::generate_user_token(&app, path.into_inner(), admin.id).await?))
}

pub async fn delete_token(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(svc::delete_user_token(&app, path.into_inner(), admin.id).await?))
}

pub async fn delete(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(svc::delete_user(&app, path.into_inner(), admin.id).await?))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/users", web::get().to(list))
        .route("/admin/users", web::post().to(create))
        .route("/admin/users/{user_id}", web::get().to(detail))
        .route("/admin/users/{user_id}/enable", web::put().to(enable))
        .route("/admin/users/{user_id}/disable", web::put().to(disable))
        .route("/admin/users/{user_id}/reset-password", web::post().to(reset_password))
        .route("/admin/users/{user_id}/allowed-dictionaries", web::put().to(set_allowed_dictionaries))
        .route("/admin/users/{user_id}/token", web::post().to(generate_token))
        .route("/admin/users/{user_id}/token", web::delete().to(delete_token))
        .route("/admin/users/{user_id}", web::delete().to(delete));
}

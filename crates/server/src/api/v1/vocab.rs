//! /api/v1/vocab —— 移植自 `app/api/v1/vocab.py`（Token 生词本；用户 Token 归属到用户生词本）。

use actix_web::web;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::ApiTokenAuth;
use crate::core::errors::AppError;
use crate::services::vocab_service;
use crate::AppState;

/// 归属解析：用户 Token → 该用户的 user 生词本；普通 Token → token 生词本
fn owner_kind(token: &crate::entities::api_token::Model) -> vocab_service::OwnerKind {
    match token.user_id {
        Some(_) => vocab_service::OwnerKind::User,
        None => vocab_service::OwnerKind::Token,
    }
}

fn owner_id(token: &crate::entities::api_token::Model) -> i32 {
    token.user_id.unwrap_or(token.id)
}

#[derive(Deserialize)]
pub struct VocabListQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 { 1 }
fn default_page_size() -> i64 { 20 }

pub async fn list(
    app: web::Data<std::sync::Arc<AppState>>,
    auth: ApiTokenAuth,
    params: web::Query<VocabListQuery>,
) -> Result<web::Json<Value>, AppError> {
    let page_size = params.page_size;
    let (items, total) = vocab_service::list_vocab_items(
        &app,
        owner_kind(&auth.0),
        owner_id(&auth.0),
        &vocab_service::VocabListParams {
            search: params.search.clone(),
            page: params.page.max(1),
            page_size: page_size.clamp(1, 100),
            sort_by: "date",
            order: "desc",
            lang_from: None,
        },
    )
    .await?;
    Ok(web::Json(json!({
        "items": items,
        "total": total,
        "page": params.page,
        "page_size": page_size,
    })))
}

#[derive(Deserialize)]
pub struct VocabCreateRequest {
    pub word: String,
    pub dictionary_id: Option<i32>,
    pub note: Option<String>,
}

pub async fn add(
    app: web::Data<std::sync::Arc<AppState>>,
    auth: ApiTokenAuth,
    body: web::Json<VocabCreateRequest>,
) -> Result<web::Json<Value>, AppError> {
    let word_len = body.word.trim().chars().count();
    if word_len < 1 || word_len > 255 {
        return Err(AppError::validation("单词长度须为 1-255 个字符"));
    }
    let item = vocab_service::add_vocab_item(
        &app,
        owner_kind(&auth.0),
        owner_id(&auth.0),
        body.word.trim(),
        body.dictionary_id,
        body.note.as_deref().map(str::trim).filter(|n| !n.is_empty()),
    )
    .await?;
    Ok(web::Json(item))
}

pub async fn delete(
    app: web::Data<std::sync::Arc<AppState>>,
    auth: ApiTokenAuth,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    vocab_service::delete_vocab_item(&app, owner_kind(&auth.0), owner_id(&auth.0), path.into_inner()).await?;
    Ok(web::Json(json!({"ok": true})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/v1/vocab", web::get().to(list))
        .route("/v1/vocab", web::post().to(add))
        .route("/v1/vocab/{item_id}", web::delete().to(delete));
}

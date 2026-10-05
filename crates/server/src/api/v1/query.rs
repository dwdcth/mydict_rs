//! /api/v1/*（对外查询 API）—— 移植自 `app/api/v1/query.py`。

use actix_web::{web, HttpRequest};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::{get_api_caller, ApiCaller};
use crate::core::errors::AppError;
use crate::services::{query, query_log, rate_limit, web_rate_limit};
use crate::AppState;

#[derive(Deserialize)]
pub struct WordQuery {
    pub word: String,
    pub dict: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default)]
    pub full_style: bool,
    #[serde(default)]
    pub all_langs: bool,
}

/// GET /api/v1/query —— Token 鉴权或 open_access 匿名；限流在业务前
pub async fn api_query(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    params: web::Query<WordQuery>,
) -> Result<web::Json<Value>, AppError> {
    // Bearer 一律按 API Token 解析（不是 JWT）；匿名走 open_access + IP 限流
    let caller: ApiCaller = get_api_caller(&req, &app).await?;
    // 限流最先执行（空词同样计数，对齐 Python）；Token 每日 / 匿名 IP 每分钟
    enforce_api_rate_limit(&app, &caller).await?;
    let word = params.word.trim();
    if word.is_empty() {
        // 对齐 Python：空词照常写 not_found 日志后返回空结果
        query_log::log_query(
            &app,
            query_log::NewQueryLog {
                source: "api",
                token_id: caller.token.as_ref().map(|t| t.id),
                user_id: caller.user.as_ref().map(|u| u.id),
                word: String::new(),
                dictionary_id: None,
                ip: caller.ip.clone(),
                status: Some("not_found"),
                duration_ms: Some(0),
            },
        )
        .await;
        return Ok(web::Json(json!({"results": []})));
    }

    let started = std::time::Instant::now();
    let dict_ids = query::parse_dict_ids(params.dict.as_deref());
    let lang_from = params.from.as_deref().filter(|s| !s.is_empty());
    let lang_to = params.to.as_deref().filter(|s| !s.is_empty());
    let allowed = caller.allowed_dictionary_ids(&app.db).await;

    let results = query::search_word(
        &app,
        word,
        dict_ids.as_deref(),
        lang_from,
        lang_to,
        allowed.as_deref(),
        true, // v1 带释义
        params.all_langs,
    )
    .await?;

    // full_style=false（默认）：definition 由 HTML 转纯文本
    let mut items = results.as_array().cloned().unwrap_or_default();
    if !params.full_style {
        for item in items.iter_mut() {
            if let Some(definition) = item.get("definition").and_then(|d| d.as_str()) {
                item["definition"] = json!(query::html_to_plain_text(definition));
            }
        }
    }

    // 写查询日志：dictionary_id 取首条结果的
    let status = if items.is_empty() { "not_found" } else { "success" };
    let first_dict = items.first().and_then(|i| i["dictionary_id"].as_i64());
    query_log::log_query(
        &app,
        query_log::NewQueryLog {
            source: "api",
            token_id: caller.token.as_ref().map(|t| t.id),
            user_id: caller.user.as_ref().map(|u| u.id),
            word: word.to_string(),
            dictionary_id: first_dict.map(|v| v as i32),
            ip: caller.ip.clone(),
            status: Some(status),
            duration_ms: Some(started.elapsed().as_millis() as i32),
        },
    )
    .await;
    Ok(web::Json(json!({"results": items})))
}

#[derive(Deserialize)]
pub struct SuggestQuery {
    pub prefix: String,
    #[serde(default = "default_suggest_limit")]
    pub limit: usize,
    pub dict: Option<String>,
}

fn default_suggest_limit() -> usize {
    10
}

/// GET /api/v1/suggest —— 限流与 /query 共用同一套
pub async fn api_suggest(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    params: web::Query<SuggestQuery>,
) -> Result<web::Json<Value>, AppError> {
    let caller: ApiCaller = get_api_caller(&req, &app).await?;
    enforce_api_rate_limit(&app, &caller).await?;
    let prefix = params.prefix.trim();
    if prefix.is_empty() {
        return Ok(web::Json(json!({"words": []})));
    }
    match &caller.token {
        Some(token) => {
            rate_limit::check_and_increment_token_daily(
                &app,
                token.daily_limit.map(|v| v as i64),
                token.id,
            )
            .await?;
        }
        None => {
            web_rate_limit::enforce_api_anonymous_rate(&app, caller.ip.as_deref().unwrap_or("unknown"))
                .await?;
        }
    }
    let limit = params.limit.min(50); // 对齐 Python min(limit,50)：0 → 空结果
    let dict_ids = query::parse_dict_ids(params.dict.as_deref());
    let allowed = caller.allowed_dictionary_ids(&app.db).await;
    let words = query::suggest_prefix(&app.db, prefix, dict_ids.as_deref(), limit, allowed.as_deref()).await?;
    Ok(web::Json(json!({"words": words})))
}

/// /api/v1/* 共用的限流口径：Token 每日配额 / 匿名 IP 每分钟（对齐 Python _enforce_rate_limit）
pub async fn enforce_api_rate_limit(
    app: &std::sync::Arc<AppState>,
    caller: &ApiCaller,
) -> Result<(), AppError> {
    match &caller.token {
        Some(token) => {
            rate_limit::check_and_increment_token_daily(
                app,
                token.daily_limit.map(|v| v as i64),
                token.id,
            )
            .await
        }
        None => {
            web_rate_limit::enforce_api_anonymous_rate(
                app,
                caller.ip.as_deref().unwrap_or("unknown"),
            )
            .await
        }
    }
}

/// GET /api/v1/dictionaries —— 调用方可用的启用词典
pub async fn api_dictionaries(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<Vec<Value>>, AppError> {
    let caller: ApiCaller = get_api_caller(&req, &app).await?;
    // 对齐 Python：词典列表同样消耗配额
    enforce_api_rate_limit(&app, &caller).await?;
    let allowed = caller.allowed_dictionary_ids(&app.db).await;
    let dicts = query::list_public_dictionaries(&app.db, allowed.as_deref()).await?;
    Ok(web::Json(
        dicts
            .iter()
            .map(|d| {
                json!({
                    "id": d.id,
                    "name": d.name,
                    "lang_from": d.lang_from,
                    "lang_to": d.lang_to,
                })
            })
            .collect(),
    ))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/v1/query", web::get().to(api_query))
        .route("/v1/suggest", web::get().to(api_suggest))
        .route("/v1/dictionaries", web::get().to(api_dictionaries));
}

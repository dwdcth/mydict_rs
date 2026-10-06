//! /api/vocab/* —— 移植自 `app/api/web/vocab.py`（登录用户生词本 + 词条文档渲染）。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::UserAuth;
use crate::core::errors::AppError;
use crate::services::{entry_render, query as query_service, vocab_service};
use crate::AppState;

#[derive(Deserialize)]
pub struct VocabListQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_order")]
    pub order: String,
    #[serde(default)]
    pub lang_from: Option<String>,
}

fn default_page() -> i64 { 1 }
fn default_page_size() -> i64 { 20 }
fn default_sort() -> String { "date".to_string() }
fn default_order() -> String { "desc".to_string() }

pub async fn list(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
    params: web::Query<VocabListQuery>,
) -> Result<web::Json<Value>, AppError> {
    // page_size 回显原值，夹只作用于查询（对齐 Python）
    let page_size = params.page_size;
    let query_page_size = page_size.clamp(1, 100);
    let (items, total) = vocab_service::list_vocab_items(
        &app,
        vocab_service::OwnerKind::User,
        user.0.id,
        &vocab_service::VocabListParams {
            search: params.search.clone(),
            page: params.page.max(1),
            page_size: query_page_size,
            sort_by: if params.sort == "word" { "word" } else { "date" },
            order: if params.order == "asc" { "asc" } else { "desc" },
            lang_from: params.lang_from.clone(),
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

pub async fn languages(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
) -> Result<web::Json<Vec<String>>, AppError> {
    Ok(web::Json(
        vocab_service::list_owner_languages(&app, vocab_service::OwnerKind::User, user.0.id).await?,
    ))
}

#[derive(Deserialize)]
pub struct VocabCreateRequest {
    pub word: String,
    pub dictionary_id: Option<i32>,
    pub note: Option<String>,
}

pub async fn add(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
    body: web::Json<VocabCreateRequest>,
) -> Result<web::Json<Value>, AppError> {
    let word_len = body.word.trim().chars().count();
    if word_len < 1 || word_len > 255 {
        return Err(AppError::validation("单词长度须为 1-255 个字符"));
    }
    let note_len = body.note.as_deref().map(|n| n.chars().count()).unwrap_or(0);
    if note_len > 65535 {
        return Err(AppError::validation("备注过长"));
    }
    let item = vocab_service::add_vocab_item(
        &app,
        vocab_service::OwnerKind::User,
        user.0.id,
        body.word.trim(),
        body.dictionary_id,
        body.note.as_deref().map(str::trim).filter(|n| !n.is_empty()),
    )
    .await?;
    Ok(web::Json(item))
}

pub async fn delete(
    app: web::Data<std::sync::Arc<AppState>>,
    user: UserAuth,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    vocab_service::delete_vocab_item(&app, vocab_service::OwnerKind::User, user.0.id, path.into_inner()).await?;
    Ok(web::Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct EntryQuery {
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String { String::new() }

/// GET /api/vocab/{item_id}/entry —— 渲染收藏时的释义快照（@@@LINK 兜底解引用）。
/// 词典还在则注入同名 css/js；allow_lookup=false（没有查词框）。
pub async fn entry(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    user: UserAuth,
    path: web::Path<i32>,
    params: web::Query<EntryQuery>,
) -> Result<HttpResponse, AppError> {
    let item = vocab_service::get_vocab_item(&app, vocab_service::OwnerKind::User, user.0.id, path.into_inner()).await?;
    let item_obj = item.as_object().cloned().unwrap_or_default();
    let word = item_obj.get("word").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let definition = item_obj
        .get("definition")
        .and_then(|v| v.as_str())
        .map(String::from);
    let dictionary_id = item_obj.get("dictionary_id").and_then(|v| v.as_i64()).map(|v| v as i32);

    // 老快照可能存着 @@@LINK= 标记本身：兜底解引用
    let definition = match dictionary_id {
        Some(dict_id) => {
            query_service::resolve_link_definition(&app, Some(dict_id), definition.as_deref())
                .await?
                .unwrap_or_default()
        }
        None => definition.unwrap_or_default(),
    };
    // 对齐 Python：空快照渲染空文档（而不是 404——收藏项本身还在）

    // 同名 css/js（词典还在才注入）
    let mut extra_assets: Vec<(String, String)> = Vec::new();
    if let Some(dict_id) = dictionary_id {
        let sources = crate::services::dictionary::source_paths_for(&app.db, dict_id).await.unwrap_or_default();
        let res_dir = std::path::Path::new(&app.cfg.dictionary_storage_path)
            .join(dict_id.to_string())
            .join("res");
        for source in &sources {
            if let Some(name) = source.file_name().and_then(|n| n.to_str()) {
                if name.to_lowercase().ends_with(".mdx") {
                    extra_assets = crate::services::mdd_resources::same_name_assets_with_mdd(
                        &app, dict_id, &res_dir, name,
                    )
                    .await;
                }
            }
        }
    }

    let render_entries = [entry_render::RenderEntry {
        word: &word,
        definition: &definition,
        phonetic: item_obj.get("phonetic").and_then(|v| v.as_str()),
    }];
    let theme = if params.theme.is_empty() { None } else { Some(params.theme.as_str()) };
    let doc = entry_render::render_entries_document(&render_entries, dictionary_id.unwrap_or(0), theme, false, &extra_assets);
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(doc))
}

/// GET /api/vocab/export —— 生词本导出 Anki 可导入的 TSV
/// （word \t 释义纯文本 \t 来源词典 三列；文件分隔符导入手选 Tab）
pub async fn export_anki(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    user: UserAuth,
) -> Result<HttpResponse, AppError> {
    use sea_orm::ConnectionTrait;
    let rows = app
        .db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            app.db.get_database_backend(),
            "SELECT v.word, v.definition, d.name AS dictionary_name \
             FROM vocab_items v LEFT JOIN dictionaries d ON d.id = v.dictionary_id \
             WHERE v.user_id = $1 ORDER BY v.created_at DESC, v.id DESC",
            [user.0.id.into()],
        ))
        .await
        .map_err(AppError::from)?;
    // Anki 导入不带头行（带反而要手动跳过）；三列：词 / 释义纯文本 / 来源
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let word = row.try_get::<String>("", "word").unwrap_or_default();
        let definition = row.try_get::<Option<String>>("", "definition")
            .ok()
            .flatten()
            .unwrap_or_default();
        let source = row
            .try_get::<Option<String>>("", "dictionary_name")
            .ok()
            .flatten()
            .unwrap_or_default();
        let plain = crate::services::query::html_to_plain_text(&definition);
        // TSV 转义：制表/换行压成空格
        let esc = |s: &str| -> String {
            s.replace('\t', " ").replace('\r', " ").replace('\n', " ")
        };
        out.push(format!("{}\t{}\t{}", esc(&word), esc(&plain), esc(&source)));
    }
    let body = out.join("\n") + "\n";
    Ok(HttpResponse::Ok()
        .content_type("text/tab-separated-values; charset=utf-8")
        .insert_header((
            actix_web::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"vocab-anki.tsv\"",
        ))
        .body(body))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/vocab", web::get().to(list))
        .route("/vocab/export", web::get().to(export_anki))
        .route("/vocab/languages", web::get().to(languages))
        .route("/vocab", web::post().to(add))
        .route("/vocab/{item_id}", web::delete().to(delete))
        .route("/vocab/{item_id}/entry", web::get().to(entry));
}

#[allow(unused)]
fn _marker(_: Option<&HttpRequest>) {}

//! /api/admin/dictionaries —— 移植自 `app/api/admin/dictionaries.py`。

use actix_multipart::Multipart;
use actix_web::{web, HttpRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::core::deps::require_admin;
use crate::core::errors::AppError;
use crate::services::dictionary as dict_service;
use crate::AppState;

// ── 列表 ─────────────────────────────────────────────────────────

pub async fn list(app: web::Data<std::sync::Arc<AppState>>, req: HttpRequest) -> Result<web::Json<Vec<Value>>, AppError> {
    require_admin(&req, &app).await?;
    let dicts = dict_service::list_dictionaries(&app.db).await?;
    Ok(web::Json(dicts.iter().map(dict_service::dict_to_json).collect()))
}

// ── 上传导入（multipart 流式落盘）────────────────────────────────

pub async fn upload(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    mut payload: Multipart,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let staging_root = Path::new(&app.cfg.dictionary_storage_path).join("_staging");
    let staging = staging_root.join(Uuid::new_v4().simple().to_string());
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| AppError::internal("staging", e))?;

    let mut name = String::new();
    let mut format = String::new();
    let mut lang_from: Option<String> = None;
    let mut lang_to: Option<String> = None;
    let mut mode = dict_service::DEFAULT_IMPORT_MODE.to_string();
    let mut saved: Vec<PathBuf> = Vec::new();
    let mut total_bytes: i64 = 0;
    let max_bytes = app.cfg.max_upload_size_mb * 1024 * 1024;

    let result = async {
        use futures::StreamExt;
        while let Some(item) = payload.next().await {
            let mut field = item.map_err(|e| AppError::validation(format!("上传数据无效：{e}")))?;
            let field_name = field.name().unwrap_or_default().to_string();
            let filename = field
                .content_disposition()
                .and_then(|d| d.get_filename())
                .map(|f| {
                    // 只取 basename（前端可能带路径）
                    Path::new(f)
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                });

            match filename {
                Some(filename) if !filename.is_empty() => {
                    // 文件字段：流式写盘
                    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
                        return Err(AppError::validation(format!("非法文件名：{filename}")));
                    }
                    let target = staging.join(&filename);
                    let mut file = tokio::fs::File::create(&target)
                        .await
                        .map_err(|e| AppError::internal("staging", e))?;
                    use tokio::io::AsyncWriteExt;
                    while let Some(chunk) = field.next().await {
                        let chunk = chunk.map_err(|e| AppError::validation(format!("上传中断：{e}")))?;
                        total_bytes += chunk.len() as i64;
                        if total_bytes > max_bytes {
                            return Err(AppError::validation(format!(
                                "上传文件总大小超过 {}MB 上限",
                                app.cfg.max_upload_size_mb
                            )));
                        }
                        file.write_all(&chunk)
                            .await
                            .map_err(|e| AppError::internal("staging", e))?;
                    }
                    saved.push(target);
                }
                _ => {
                    // 表单字段：读全值（都很短）
                    let mut buf: Vec<u8> = Vec::new();
                    while let Some(chunk) = field.next().await {
                        let chunk = chunk.map_err(|e| AppError::validation(format!("上传中断：{e}")))?;
                        buf.extend_from_slice(&chunk);
                        if buf.len() > 4096 {
                            return Err(AppError::validation("表单字段过长"));
                        }
                    }
                    let value = String::from_utf8_lossy(&buf).to_string();
                    match field_name.as_str() {
                        "name" => name = value,
                        "format" => format = value,
                        "lang_from" => lang_from = Some(value),
                        "lang_to" => lang_to = Some(value),
                        "mode" => mode = value,
                        _ => {} // 未知字段忽略
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    if let Err(err) = result {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(err);
    }
    if saved.is_empty() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(AppError::validation("未收到任何文件"));
    }
    let name_len = name.trim().chars().count();
    if name_len < 1 || name_len > 255 {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(AppError::validation("词典名称长度须为 1-255 个字符"));
    }

    let task_id = match dict_service::start_dictionary_import(
        &app,
        dict_service::ImportParams {
            name: name.trim().to_string(),
            format,
            lang_from: lang_from.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
            lang_to: lang_to.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
            staged_paths: saved,
            admin_id: admin.id,
            import_method: "upload".to_string(),
            skip_resources: false,
            extract_resources: false, // 磁盘优化：默认不解包，/dict-res 直接读 .mdd
            mode,
        },
    ) {
        Ok(task_id) => task_id,
        Err(err) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(err);
        }
    };
    Ok(web::Json(json!({"task_id": task_id})))
}

// ── 浏览器上传（多文件/文件夹/压缩包 → 自动分组导入）──────────────

/// POST /api/admin/dictionaries/analyze-upload（multipart）
/// 先把文件（含文件夹相对路径）落到暂存区、解压 .zip、按现有扫描逻辑分组返回，
/// 不导入。前端确认分组后带 upload_id 调 import-uploaded。
pub async fn analyze_upload(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    mut payload: Multipart,
) -> Result<web::Json<Value>, AppError> {
    let _admin = require_admin(&req, &app).await?;
    let upload_id = Uuid::new_v4().simple().to_string();
    let staging = dict_service::upload_staging_dir(&app.cfg, &upload_id);
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|e| AppError::internal("staging", e))?;
    let _ = tokio::task::spawn_blocking({
        let cfg = app.cfg.clone();
        move || dict_service::prune_stale_staging(&cfg)
    })
    .await;

    let mut total_bytes: i64 = 0;
    let max_bytes = app.cfg.max_upload_size_mb * 1024 * 1024;
    let result = async {
        use futures::StreamExt;
        while let Some(item) = payload.next().await {
            let mut field = item.map_err(|e| AppError::validation(format!("上传数据无效：{e}")))?;
            let Some(filename) = field
                .content_disposition()
                .and_then(|d| d.get_filename())
                .map(|f| f.replace('\\', "/"))
            else {
                // 非文件字段全部跳过（分析接口不需要表单值）
                while let Some(chunk) = field.next().await {
                    chunk.map_err(|e| AppError::validation(format!("上传中断：{e}")))?;
                }
                continue;
            };
            let target = dict_service::safe_staging_path(&staging, &filename)?;
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| AppError::internal("staging", e))?;
            }
            let mut file = tokio::fs::File::create(&target)
                .await
                .map_err(|e| AppError::internal("staging", e))?;
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = field.next().await {
                let chunk = chunk.map_err(|e| AppError::validation(format!("上传中断：{e}")))?;
                total_bytes += chunk.len() as i64;
                if total_bytes > max_bytes {
                    return Err(AppError::validation(format!(
                        "上传文件总大小超过 {}MB 上限",
                        app.cfg.max_upload_size_mb
                    )));
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|e| AppError::internal("staging", e))?;
            }
        }
        Ok(())
    }
    .await;
    if let Err(err) = result {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(err);
    }

    let analysis = dict_service::analyze_upload_staging(&app, &upload_id).await?;
    Ok(web::Json(analysis))
}

#[derive(Deserialize)]
pub struct ImportUploadedRequest {
    pub upload_id: String,
    pub name: String,
    pub format: String,
    pub lang_from: Option<String>,
    pub lang_to: Option<String>,
    /// full：释义落库；lite（默认）：只落词头，释义运行期物化
    #[serde(default = "default_import_mode")]
    pub mode: String,
    pub files: Vec<String>,
}

/// POST /api/admin/dictionaries/import-uploaded
/// 导入 analyze-upload 分组里的一部（文件从暂存区移入词典 source/ 归档）。
pub async fn import_uploaded(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<ImportUploadedRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let name_len = body.name.trim().chars().count();
    if name_len < 1 || name_len > 255 {
        return Err(AppError::validation("词典名称长度须为 1-255 个字符"));
    }
    for lang in [&body.lang_from, &body.lang_to].into_iter().flatten() {
        if lang.trim().is_empty() || lang.trim().chars().count() > 8 {
            return Err(AppError::validation("语言代码须为 1-8 个字符"));
        }
    }
    let mut staged =
        dict_service::resolve_uploaded_files(&app.cfg, &body.upload_id, &body.files)?;
    dict_service::append_sibling_assets_pub(&mut staged);
    let task_id = dict_service::start_dictionary_import(
        &app,
        dict_service::ImportParams {
            name: body.name.trim().to_string(),
            format: body.format.clone(),
            lang_from: body
                .lang_from
                .clone()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            lang_to: body
                .lang_to
                .clone()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            staged_paths: staged,
            admin_id: admin.id,
            import_method: "upload".to_string(),
            skip_resources: false,
            extract_resources: false,
            mode: body.mode.clone(),
        },
    )?;
    Ok(web::Json(json!({"task_id": task_id})))
}

// ── dicts_dir 扫描与导入 ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct DictsDirFilesQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

pub async fn dicts_dir_files(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    query: web::Query<DictsDirFilesQuery>,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    let (path, entries, dictionaries, skipped) =
        dict_service::list_dicts_dir_files(&app, &query.path, query.recursive).await?;
    Ok(web::Json(json!({
        "path": path,
        "entries": entries,
        "dictionaries": dictionaries,
        "skipped": skipped,
    })))
}

#[derive(Deserialize)]
pub struct ImportFromDictsDirRequest {
    pub name: String,
    pub format: String,
    pub lang_from: Option<String>,
    pub lang_to: Option<String>,
    #[serde(default)]
    pub skip_resources: bool,
    /// 可选：把 .mdd 全量解包到 res/（默认 false——运行期直接读 .mdd，省磁盘）
    #[serde(default)]
    pub extract_resources: bool,
    /// full：释义落库；lite（默认）：只落词头，释义运行期物化
    #[serde(default = "default_import_mode")]
    pub mode: String,
    pub files: Vec<String>,
}

fn default_import_mode() -> String {
    dict_service::DEFAULT_IMPORT_MODE.to_string()
}

pub async fn import_from_dicts_dir(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<ImportFromDictsDirRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let name_len = body.name.trim().chars().count();
    if name_len < 1 || name_len > 255 {
        return Err(AppError::validation("词典名称长度须为 1-255 个字符"));
    }
    if let Some(lang) = &body.lang_from {
        if lang.trim().is_empty() || lang.trim().chars().count() > 8 {
            return Err(AppError::validation("lang_from 须为 1-8 个字符"));
        }
    }
    if let Some(lang) = &body.lang_to {
        if lang.trim().is_empty() || lang.trim().chars().count() > 8 {
            return Err(AppError::validation("lang_to 须为 1-8 个字符"));
        }
    }
    if body.files.is_empty() {
        return Err(AppError::validation("files 不能为空"));
    }
    let staged = dict_service::resolve_dicts_dir_files(&body.files, &app.cfg)?;
    let task_id = dict_service::start_dictionary_import(
        &app,
        dict_service::ImportParams {
            name: body.name.trim().to_string(),
            format: body.format.clone(),
            lang_from: body.lang_from.clone(),
            lang_to: body.lang_to.clone(),
            staged_paths: staged,
            admin_id: admin.id,
            import_method: "dicts_dir".to_string(),
            skip_resources: body.skip_resources,
            extract_resources: body.extract_resources,
            mode: body.mode.clone(),
        },
    )?;
    Ok(web::Json(json!({"task_id": task_id})))
}

// ── 管理端词条预览（测试查询 iframe 用）──────────────────────────

#[derive(Deserialize)]
pub struct AdminEntryQuery {
    pub word: String,
    pub theme: Option<String>,
}

/// GET /api/admin/dictionaries/{id}/entry —— 管理端预览单条词条的 HTML 文档。
/// 与前台 /api/dict/entry 的区别：**不检查启用状态**（测试查询的对象常常正是
/// 还没启用的词典），也不做用户授权过滤（管理员天然全量）。allow_lookup 关闭
/// （管理端预览不需要选中查词）。
pub async fn entry_document(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    params: web::Query<AdminEntryQuery>,
) -> Result<actix_web::HttpResponse, AppError> {
    let _admin = require_admin(&req, &app).await?;
    let dictionary_id = path.into_inner();
    let _dictionary = dict_service::get_dictionary(&app.db, dictionary_id).await?;

    let word = params.word.trim();
    if word.is_empty() {
        return Err(AppError::not_found("词条不存在"));
    }
    let entries = crate::services::query::get_entries_for_document(
        &app,
        dictionary_id,
        word,
        None,
    )
    .await?;
    if entries.is_empty() {
        return Err(AppError::not_found("词条不存在"));
    }

    // 同名 .css/.js 注入（磁盘 res/ + .mdd 兜底）
    let source_files: Vec<PathBuf> =
        crate::services::dictionary::source_paths_for(&app.db, dictionary_id)
            .await
            .unwrap_or_default();
    let res_dir = std::path::Path::new(&app.cfg.dictionary_storage_path)
        .join(dictionary_id.to_string())
        .join("res");
    let mut extra_assets: Vec<(String, String)> = Vec::new();
    for source in &source_files {
        if let Some(name) = source.file_name().and_then(|n| n.to_str()) {
            if name.to_lowercase().ends_with(".mdx") {
                extra_assets = crate::services::mdd_resources::same_name_assets_with_mdd(
                    &app, dictionary_id, &res_dir, name,
                )
                .await;
            }
        }
    }

    let render_entries: Vec<crate::services::entry_render::RenderEntry<'_>> = entries
        .iter()
        .map(|e| crate::services::entry_render::RenderEntry {
            word: &e.word,
            definition: &e.definition,
            phonetic: e.phonetic.as_deref(),
        })
        .collect();
    let doc = crate::services::entry_render::render_entries_document(
        &render_entries,
        dictionary_id,
        params.theme.as_deref(),
        false, // 管理端预览：不开选中查词
        &extra_assets,
    );
    app.query_cache
        .insert_doc(format!("admin|{dictionary_id}|{word}"), std::sync::Arc::new(doc.clone()));
    Ok(actix_web::HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(doc))
}

// ── 词条模式切换（lite↔full）─────────────────────────────────────

#[derive(Deserialize)]
pub struct EntryModeRequest {
    /// "full"：释义落库（FTS 等高级查询的前提）；"lite"：只落词头
    pub mode: String,
}

/// POST /api/admin/dictionaries/{id}/entry-mode
/// 后台按新模式重灌词条（走 bulk_write 队列，与其它导入任务串行）
pub async fn switch_entry_mode(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    body: web::Json<EntryModeRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let (task_id, changed) =
        dict_service::start_mode_switch(&app, path.into_inner(), &body.mode).await?;
    if !changed {
        return Ok(web::Json(json!({"task_id": null, "changed": false})));
    }
    let _ = admin;
    Ok(web::Json(json!({"task_id": task_id, "changed": true})))
}

// ── 批量操作 ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub ordered_ids: Vec<i32>,
}

#[derive(Deserialize)]
pub struct BatchStatusRequest {
    pub dictionary_ids: Vec<i32>,
    pub status: String,
}

#[derive(Deserialize)]
pub struct DictionaryIdsRequest {
    pub dictionary_ids: Option<Vec<i32>>,
}

#[derive(Deserialize)]
pub struct RenameDictionariesRequest {
    pub pattern: String,
    pub replacement: String,
    pub dictionary_ids: Option<Vec<i32>>,
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

pub async fn reorder(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<ReorderRequest>,
) -> Result<web::Json<Vec<Value>>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(dict_service::reorder_dictionaries(&app, &body.ordered_ids, admin.id).await?))
}

pub async fn batch_status(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<BatchStatusRequest>,
) -> Result<web::Json<Vec<Value>>, AppError> {
    let admin = require_admin(&req, &app).await?;
    if !matches!(body.status.as_str(), "enabled" | "disabled") {
        return Err(AppError::validation("status 只能是 enabled 或 disabled"));
    }
    if body.dictionary_ids.is_empty() || body.dictionary_ids.len() > 500 {
        return Err(AppError::validation("dictionary_ids 须为 1-500 个"));
    }
    Ok(web::Json(
        dict_service::set_dictionaries_status(&app, &body.dictionary_ids, &body.status, admin.id).await?,
    ))
}

pub async fn reparse(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<DictionaryIdsRequest>,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    let task_id = dict_service::start_reparse(&app, body.dictionary_ids.as_deref()).await?;
    Ok(web::Json(json!({"task_id": task_id})))
}

pub async fn rename(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<RenameDictionariesRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    Ok(web::Json(
        dict_service::rename_dictionaries(
            &app,
            &body.pattern,
            &body.replacement,
            body.dictionary_ids.as_deref(),
            body.dry_run,
            admin.id,
        )
        .await?,
    ))
}

// ── 单词典操作 ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DictionaryUpdateRequest {
    pub name: String,
    pub lang_from: String,
    pub lang_to: String,
}

pub async fn update_one(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    body: web::Json<DictionaryUpdateRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let id = path.into_inner();
    let name_len = body.name.trim().chars().count();
    if name_len < 1 || name_len > 255 {
        return Err(AppError::validation("词典名称长度须为 1-255 个字符"));
    }
    for (label, value) in [("lang_from", &body.lang_from), ("lang_to", &body.lang_to)] {
        let len = value.trim().chars().count();
        if len < 1 || len > 8 {
            return Err(AppError::validation(format!("{label} 须为 1-8 个字符")));
        }
    }
    Ok(web::Json(
        dict_service::update_dictionary_metadata(
            &app,
            id,
            body.name.trim(),
            body.lang_from.trim(),
            body.lang_to.trim(),
            admin.id,
        )
        .await?,
    ))
}

pub async fn enable(app: web::Data<std::sync::Arc<AppState>>, req: HttpRequest, path: web::Path<i32>) -> Result<web::Json<Value>, AppError> {
    set_status(app, req, path, "enabled").await
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
    Ok(web::Json(
        dict_service::set_dictionary_status(&app, path.into_inner(), status, admin.id).await?,
    ))
}

pub async fn delete_one(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    dict_service::delete_dictionary(&app, path.into_inner(), admin.id).await?;
    Ok(web::Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct TestQueryQuery {
    pub word: String,
}

pub async fn test_query(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    query: web::Query<TestQueryQuery>,
) -> Result<web::Json<Vec<Value>>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(
        dict_service::test_query(&app, path.into_inner(), &query.word, 20).await?,
    ))
}

// ── 从源文件修复 / 美音喇叭清理 ──────────────────────────────────

/// POST /api/admin/dictionaries/repair-from-source
/// 补 .mdx 同级附属资源 + 展开存量 `` `N` `` 标记（不重新导入）
pub async fn repair_from_source(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<DictionaryIdsRequest>,
) -> Result<web::Json<Value>, AppError> {
    let admin = require_admin(&req, &app).await?;
    let targets = crate::services::dictionary::resolve_target_ids_for_admin(
        &app.db, body.dictionary_ids.as_deref(),
    )
    .await?;
    let task_id = app.tasks.start("dictionary_source_repair", "从源文件修复", false);
    let state = (*app).clone();
    tokio::spawn(async move {
        let _guard = state.bulk_write.lock().await;
        let _ = admin;
        let result = run_source_repair(&state, task_id, targets).await;
        match result {
            Ok(value) => state.tasks.succeed(task_id, value),
            Err(err) => state.tasks.fail(task_id, err.message),
        }
    });
    Ok(web::Json(json!({"task_id": task_id})))
}

async fn run_source_repair(
    state: &std::sync::Arc<AppState>,
    task_id: i64,
    dictionary_ids: Vec<i32>,
) -> Result<Value, AppError> {
    use crate::services::definition_repair as repair;
    let total = dictionary_ids.len();
    let mut repaired_dicts = 0i64;
    let mut copied_files = 0i64;
    let mut styled_dictionaries = 0i64;
    let mut styled_entries = 0i64;
    // 先定位「词条里真的含反引号」的词典（打开 .mdx 要读整份词头索引，不能全开）
    let with_markers = repair::dictionaries_using_style_markers(state, &dictionary_ids).await?;
    for (idx, dict_id) in dictionary_ids.iter().enumerate() {
        let index = (idx + 1) as i64;
        let sources = crate::services::dictionary::source_paths_for(&state.db, *dict_id)
            .await
            .unwrap_or_default();
        let res_dir = std::path::Path::new(&state.cfg.dictionary_storage_path)
            .join(dict_id.to_string())
            .join("res");
        let count = dict_parser::resources::copy_sibling_resources(&res_dir, &sources);
        if count > 0 {
            repaired_dicts += 1;
        }
        copied_files += count as i64;
        if with_markers.contains(dict_id) {
            let (sheet, compact) = repair::source_style_context(&sources);
            if compact {
                let changed = repair::expand_stored_styles(
                    state, *dict_id, &sheet, true, repair::DEFAULT_BATCH_SIZE,
                )
                .await?;
                if changed > 0 {
                    styled_dictionaries += 1;
                    styled_entries += changed;
                }
            }
        }
        let mut progress = serde_json::Map::new();
        progress.insert("done".into(), json!(index));
        progress.insert("total".into(), json!(total));
        state.tasks.update_progress(task_id, progress);
    }
    if styled_entries > 0 {
        state.query_cache.invalidate();
    }
    Ok(json!({
        "dictionaries": repaired_dicts,
        "files": copied_files,
        "styled_dictionaries": styled_dictionaries,
        "styled_entries": styled_entries,
    }))
}

/// POST /api/admin/dictionaries/{id}/cleanup-uss-speakers
pub async fn cleanup_uss_speakers(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<web::Json<Value>, AppError> {
    let _admin = require_admin(&req, &app).await?;
    let dictionary_id = path.into_inner();
    // 词典不存在 → 404
    use sea_orm::ConnectionTrait;
    let exists = app
        .db
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            app.db.get_database_backend(),
            "SELECT id FROM dictionaries WHERE id = $1",
            [dictionary_id.into()],
        ))
        .await?
        .is_some();
    if !exists {
        return Err(AppError::not_found("词典不存在"));
    }
    let task_id = app.tasks.start("dictionary_uss_cleanup", "清理缺失的美音例句喇叭", false);
    let state = (*app).clone();
    let cfg_storage = state.cfg.dictionary_storage_path.clone();
    tokio::spawn(async move {
        let _guard = state.bulk_write.lock().await;
        let res_dir = std::path::Path::new(&cfg_storage)
            .join(dictionary_id.to_string())
            .join("res");
        let result = crate::services::definition_repair::remove_missing_uss_speakers(
            &state, dictionary_id, &res_dir, crate::services::definition_repair::DEFAULT_BATCH_SIZE,
        )
        .await;
        match result {
            Ok((entries, speakers)) => {
                if entries > 0 {
                    state.query_cache.invalidate();
                }
                state.tasks.succeed(task_id, json!({"entries": entries, "speakers": speakers}));
            }
            Err(err) => state.tasks.fail(task_id, err.message),
        }
    });
    Ok(web::Json(json!({"task_id": task_id})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/dictionaries", web::get().to(list))
        .route("/admin/dictionaries", web::post().to(upload))
        .route(
            "/admin/dictionaries/dicts-dir-files",
            web::get().to(dicts_dir_files),
        )
        .route(
            "/admin/dictionaries/import-from-dicts-dir",
            web::post().to(import_from_dicts_dir),
        )
        .route("/admin/dictionaries/reorder", web::put().to(reorder))
        .route("/admin/dictionaries/batch-status", web::put().to(batch_status))
        .route("/admin/dictionaries/reparse", web::post().to(reparse))
        .route("/admin/dictionaries/{id}/entry-mode", web::post().to(switch_entry_mode))
        .route("/admin/dictionaries/analyze-upload", web::post().to(analyze_upload))
        .route("/admin/dictionaries/import-uploaded", web::post().to(import_uploaded))
        .route("/admin/dictionaries/rename", web::post().to(rename))
        .route(
            "/admin/dictionaries/repair-from-source",
            web::post().to(repair_from_source),
        )
        .route("/admin/dictionaries/{id}", web::put().to(update_one))
        .route("/admin/dictionaries/{id}/enable", web::put().to(enable))
        .route("/admin/dictionaries/{id}/disable", web::put().to(disable))
        .route("/admin/dictionaries/{id}", web::delete().to(delete_one))
        .route("/admin/dictionaries/{id}/test-query", web::get().to(test_query))
        .route("/admin/dictionaries/{id}/entry", web::get().to(entry_document))
        .route(
            "/admin/dictionaries/{id}/cleanup-uss-speakers",
            web::post().to(cleanup_uss_speakers),
        );
}


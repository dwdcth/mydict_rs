//! /api/dict/* —— 移植自 `app/api/web/dict.py`（查询/词条文档/词典列表/历史）。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::deps::{get_web_caller, require_user, WebCaller};
use crate::core::errors::AppError;
use crate::services::{entry_render, query, query_log, web_rate_limit};
use crate::AppState;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub word: String,
    pub dict: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// GET /api/dict/dictionaries（需登录，不开放匿名；scope=usable|all）
#[derive(Deserialize)]
pub struct DictionariesQuery {
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "usable".to_string()
}

pub async fn dictionaries(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    query_params: web::Query<DictionariesQuery>,
) -> Result<web::Json<Vec<Value>>, AppError> {
    let user = require_user(&req, &app).await?;
    // usable = 用户实际可用（管理员上限∩自选）；all = 管理员上限内全部（不看自选）
    let allowed_ids: Option<Vec<i32>> = if query_params.scope == "all" {
        if user.admin_scope_limited != 0 {
            Some(crate::services::scope::admin_granted_ids(&app.db, user.id).await?)
        } else {
            None
        }
    } else {
        crate::core::deps::user_allowed_dictionary_ids(&app.db, &user).await
    };
    let dicts = query::list_public_dictionaries(&app.db, allowed_ids.as_deref()).await?;
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

/// GET /api/dict/search —— 限流在最前；结果不带释义（释义另走 /dict/entry）
pub async fn search(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    params: web::Query<SearchQuery>,
) -> Result<web::Json<Value>, AppError> {
    let caller: WebCaller = get_web_caller(&req, &app).await?;
    let logged_in = caller.user.is_some();
    let user_id = caller.user.as_ref().map(|u| u.id);
    web_rate_limit::enforce_search_rate(&app, logged_in, &caller.ip, user_id).await?;

    let word = params.word.trim();
    if word.is_empty() {
        return Ok(web::Json(json!({"results": []})));
    }
    let started = std::time::Instant::now();
    let dict_ids = query::parse_dict_ids(params.dict.as_deref());
    let lang_from = params.from.as_deref().filter(|s| !s.is_empty());
    let lang_to = params.to.as_deref().filter(|s| !s.is_empty());
    let allowed = crate::core::deps::caller_allowed_ids(&app, caller.user.as_ref()).await?;

    let results = query::search_word(
        &app,
        word,
        dict_ids.as_deref(),
        lang_from,
        lang_to,
        allowed.as_deref(),
        false, // web 搜索不带释义
        false,
    )
    .await?;

    // 写查询日志（status/dictionary_id 取首条结果）
    let items = results.as_array().cloned().unwrap_or_default();
    let status = if items.is_empty() { "not_found" } else { "success" };
    let first_dict = items.first().and_then(|i| i["dictionary_id"].as_i64());
    query_log::log_query(
        &app,
        query_log::NewQueryLog {
            source: "web",
            token_id: None,
            user_id,
            word: word.to_string(),
            dictionary_id: first_dict.map(|v| v as i32),
            ip: Some(caller.ip.clone()),
            status: Some(status),
            duration_ms: Some(started.elapsed().as_millis() as i32),
        },
    )
    .await;
    Ok(web::Json(json!({"results": results})))
}

#[derive(Deserialize)]
pub struct EntryQuery {
    pub word: String,
    /// "12,34,56"：查询结果里那组词条的条目 id（≤200，去重排序；全无效回退按词取）
    pub entry_ids: Option<String>,
    pub theme: Option<String>,
}

/// GET /api/dict/entry/{dictionary_id} —— 词条 HTML 文档（iframe 用）。
/// 统一 404 不区分「词典不存在/未启用/不在授权范围/词条不存在」，防枚举。
pub async fn entry(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    params: web::Query<EntryQuery>,
) -> Result<HttpResponse, AppError> {
    let caller = get_web_caller(&req, &app).await?;
    web_rate_limit::enforce_entry_rate(&app, caller.user.is_some(), &caller.ip).await?;

    let dictionary_id = path.into_inner();
    // 授权范围校验（404 统一口径）
    let dict_row = {
        use sea_orm::EntityTrait;
        crate::entities::dictionary::Entity::find_by_id(dictionary_id)
            .one(&app.db)
            .await
            .map_err(AppError::from)?
    };
    let allowed = crate::core::deps::caller_allowed_ids(&app, caller.user.as_ref()).await?;
    let dict = match dict_row {
        Some(d) if d.status == "enabled" => d,
        _ => return Err(AppError::not_found("词条不存在")),
    };
    if let Some(allowed) = &allowed {
        if !allowed.contains(&dict.id) {
            return Err(AppError::not_found("词条不存在"));
        }
    }

    let word = params.word.trim();
    if word.is_empty() {
        return Err(AppError::not_found("词条不存在"));
    }
    // theme 枚举校验（对齐 Python Literal["light","dark"]）
    if let Some(theme) = params.theme.as_deref() {
        if !matches!(theme, "light" | "dark") {
            return Err(AppError::validation("theme 只能是 light 或 dark"));
        }
    }
    // entry_ids 解析：任一段非法就整体回退按词路径（对齐 Python 的 int() 全有全无）
    let entry_ids: Option<Vec<i32>> = params.entry_ids.as_deref().and_then(|raw| {
        let ids: Vec<i32> = raw
            .split(',')
            .map(|p| p.trim().parse::<i32>())
            .collect::<Result<Vec<_>, _>>()
            .ok()?
            .into_iter()
            .filter(|id| *id > 0)
            .collect();
        if ids.is_empty() {
            None
        } else {
            let mut ids = ids;
            ids.sort_unstable();
            ids.dedup();
            ids.truncate(200);
            Some(ids)
        }
    });

    let entries = query::get_entries_for_document(
        &app,
        dictionary_id,
        word,
        entry_ids.as_deref(),
    )
    .await?;
    if entries.is_empty() {
        return Err(AppError::not_found("词条不存在"));
    }

    // 同名 .css/.js 注入（词条里已引用的会被过滤）
    let source_files: Vec<std::path::PathBuf> =
        crate::services::dictionary::source_paths_for(&app.db, dictionary_id)
            .await
            .unwrap_or_default();
    let res_dir = std::path::Path::new(&app.cfg.dictionary_storage_path)
        .join(dictionary_id.to_string())
        .join("res");
    let mut extra_assets = Vec::new();
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

    // 短 TTL 文档缓存：图片版词典一份文档 35KB+ 且渲染要物化/mdd 探测；
    // key 含 entry_ids 与主题（两个主题变体各存各的）
    let doc_key = format!(
        "{dictionary_id}|{}|{}|{}",
        word,
        entry_ids.as_ref().map(|ids| ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")).unwrap_or_default(),
        params.theme.as_deref().unwrap_or("")
    );
    if let Some(cached) = app.query_cache.get_doc(&doc_key) {
        return Ok(HttpResponse::Ok()
            .content_type("text/html; charset=utf-8")
            .body((*cached).clone()));
    }
    let render_entries: Vec<entry_render::RenderEntry<'_>> = entries
        .iter()
        .map(|e| entry_render::RenderEntry {
            word: &e.word,
            definition: &e.definition,
            phonetic: e.phonetic.as_deref(),
        })
        .collect();
    let doc = entry_render::render_entries_document(
        &render_entries,
        dictionary_id,
        params.theme.as_deref(),
        true, // 前台查询页：允许选中查词
        &extra_assets,
    );
    app.query_cache
        .insert_doc(doc_key, std::sync::Arc::new(doc.clone()));
    Ok(HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(doc))
}

/// GET /api/dict/history（登录用户最近 100 条成功查询）
pub async fn history(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<Value>, AppError> {
    let user = require_user(&req, &app).await?;
    let items = query_log::get_recent_history(&app, user.id).await?;
    Ok(web::Json(json!({"items": items})))
}

/// GET /api/dict/word-of-the-day —— 今日一词（日期种子确定性取样，同日恒定）
pub async fn word_of_the_day(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<Value>, AppError> {
    let caller = get_web_caller(&req, &app).await?;
    let allowed = crate::core::deps::caller_allowed_ids(&app, caller.user.as_ref()).await?;
    use sea_orm::ConnectionTrait;

    // 干净词头过滤（对齐浏览模式）：2-50 字符；含数字的词在 Rust 侧跳过
    // （GLOB 是 SQLite 方言，多库兼容起见不做 SQL 侧字符类过滤）
    let clean = "length(e.word) BETWEEN 2 AND 50";
    let (where_allowed, values_allowed): (String, Vec<sea_orm::Value>) = match &allowed {
        Some(ids) if !ids.is_empty() => {
            let backend = app.db.get_database_backend();
            let ph = crate::services::query::sql_placeholders(backend, ids.len());
            (
                format!(" AND e.dictionary_id IN ({ph})"),
                ids.iter().map(|id| (*id).into()).collect(),
            )
        }
        Some(_) => (" AND 0".to_string(), Vec::new()),
        None => (String::new(), Vec::new()),
    };

    // 日期种子：MD5(本地日期) 取前 8 hex → u64（同日所有请求同值，跨日自动换）
    let zone = crate::core::timeutil::local_zone(&app.cfg.timezone);
    let today = crate::core::timeutil::today_str(zone);
    let digest = md5_like_seed(&today);
    let backend = app.db.get_database_backend();

    // 落点 = 种子 % 主键跨度，向后找第一条合格词；到尾则回头
    let bounds = app
        .db
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            backend,
            &format!(
                "SELECT MIN(e.id) AS lo, MAX(e.id) AS hi FROM dict_entries e \
                 JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
                 WHERE d.status = 'enabled'{where_allowed}"
            ),
            values_allowed.clone(),
        ))
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("没有可用的词典"))?;
    let (lo, hi) = (
        bounds.try_get::<Option<i64>>("", "lo").ok().flatten(),
        bounds.try_get::<Option<i64>>("", "hi").ok().flatten(),
    );
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Err(AppError::not_found("没有可用的词条"));
    };
    let span = (hi - lo + 1).max(1);
    let landing = lo + (digest % span as u64) as i64;

    let sql = format!(
        "SELECT e.word, e.dictionary_id, d.name AS dictionary_name FROM dict_entries e \
         JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
         WHERE d.status = 'enabled' AND {clean}{where_allowed} AND e.id >= $1 \
         ORDER BY e.id LIMIT 20"
    );
    let pick_rows = |id: i64| -> Vec<sea_orm::QueryResult> {
        let mut values = vec![id.into()];
        values.extend(values_allowed.clone());
        let stmt = sea_orm::Statement::from_sql_and_values(backend, &sql, values);
        futures::executor::block_on(app.db.query_all_raw(stmt))
            .unwrap_or_default()
    };
    // 落点向后最多看 20 条，跳过含数字的词头；落点太靠尾（后面全不干净）就环绕回开头再找
    fn clean_of(rows: &[sea_orm::QueryResult]) -> Option<&sea_orm::QueryResult> {
        rows.iter().find(|r| {
            let w = r.try_get::<String>("", "word").unwrap_or_default();
            !w.chars().any(|c| c.is_ascii_digit())
        })
    }
    let mut rows = pick_rows(landing);
    let mut row = clean_of(&rows);
    if row.is_none() {
        // 落点在尾部且后面全不干净 → 环绕回开头
        rows = pick_rows(lo);
        row = clean_of(&rows);
    }
    let row = row.or_else(|| rows.first());
    let Some(row) = row else {
        return Err(AppError::not_found("没有可用的词条"));
    };
    Ok(web::Json(json!({
        "word": row.try_get::<String>("", "word").unwrap_or_default(),
        "dictionary_id": row.try_get::<i32>("", "dictionary_id").unwrap_or_default(),
        "dictionary_name": row.try_get::<String>("", "dictionary_name").unwrap_or_default(),
        "date": today,
    })))
}

/// 日期串 → 稳定散列种子（FNV-1a 64：无依赖、分布足够）
fn md5_like_seed(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/dict/dictionaries", web::get().to(dictionaries))
        .route("/dict/search", web::get().to(search))
        .route("/dict/entry/{dictionary_id}", web::get().to(entry))
        .route("/dict/history", web::get().to(history))
        .route("/dict/word-of-the-day", web::get().to(word_of_the_day));
}

//! 学习工具端点：词频分析、词条浏览。

use actix_multipart::Multipart;
use actix_web::{web, HttpRequest};
use serde::Deserialize;
use serde_json::json;

use crate::core::deps::get_web_caller;
use crate::core::errors::AppError;
use crate::AppState;

#[derive(Deserialize)]
pub struct WordFrequencyRequest {
    pub text: String,
}

/// POST /api/tools/word-frequency —— 贴文本分析（multipart 版支持 .txt/.epub 上传）
pub async fn word_frequency(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    body: web::Json<WordFrequencyRequest>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let caller = get_web_caller(&req, &app).await?;
    let user_id = caller.user.as_ref().map(|u| u.id);
    Ok(web::Json(
        crate::services::word_freq::analyze(&app, user_id, &body.text).await?,
    ))
}

/// POST /api/tools/word-frequency-upload —— 上传 .txt/.epub 分析
pub async fn word_frequency_upload(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    mut payload: Multipart,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let caller = get_web_caller(&req, &app).await?;
    let user_id = caller.user.as_ref().map(|u| u.id);
    let mut text: Option<String> = None;
    let mut total = 0i64;
    let max_bytes = app.cfg.max_upload_size_mb * 1024 * 1024;
    use futures::StreamExt;
    while let Some(item) = payload.next().await {
        let mut field = item.map_err(|e| AppError::validation(format!("上传数据无效：{e}")))?;
        let filename = field
            .content_disposition()
            .and_then(|d| d.get_filename())
            .map(|f| f.to_lowercase())
            .unwrap_or_default();
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = field.next().await {
            let chunk = chunk.map_err(|e| AppError::validation(format!("上传中断：{e}")))?;
            total += chunk.len() as i64;
            if total > max_bytes {
                return Err(AppError::validation("文件过大"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if filename.ends_with(".epub") || filename.ends_with(".zip") {
            let extracted = crate::services::word_freq::extract_epub_text(&bytes)
                .ok_or_else(|| AppError::validation("无法读取 EPUB（不是合法的 zip 容器）"))?;
            text = Some(extracted);
        } else {
            // .txt / 无后缀：按 UTF-8（带 BOM 容错）读
            let slice = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
            text = Some(String::from_utf8_lossy(slice).into_owned());
        }
        break; // 只取第一个文件字段
    }
    let Some(text) = text else {
        return Err(AppError::validation("未收到文件"));
    };
    Ok(web::Json(
        crate::services::word_freq::analyze(&app, user_id, &text).await?,
    ))
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    /// 游标：word_lower > after（首页省略）
    #[serde(default)]
    pub after: String,
    #[serde(default = "default_browse_limit")]
    pub limit: i64,
}

fn default_browse_limit() -> i64 {
    200
}

/// GET /api/dict/browse/{dictionary_id} —— 词条浏览（A-Z 翻阅模式）：
/// 干净词头（2-50 字符、不含数字）按序分页，像纸质词典从头翻到尾
pub async fn browse(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<i32>,
    query: web::Query<BrowseQuery>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    use sea_orm::{ConnectionTrait, EntityTrait};
    let caller = get_web_caller(&req, &app).await?;
    let dictionary_id = path.into_inner();
    let dict = crate::entities::dictionary::Entity::find_by_id(dictionary_id)
        .one(&app.db)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("词典不存在"))?;
    if dict.status != "enabled" {
        return Err(AppError::not_found("词典不存在"));
    }
    let allowed = crate::core::deps::caller_allowed_ids(&app, caller.user.as_ref()).await?;
    if let Some(allowed) = &allowed {
        if !allowed.contains(&dictionary_id) {
            return Err(AppError::not_found("词典不存在"));
        }
    }

    let limit = query.limit.clamp(1, 500);
    let backend = app.db.get_database_backend();
    // 数字过滤用 10 个 NOT LIKE（方言中立；比正则/GLOB 可移植）
    let mut digit_filter = String::new();
    for d in '0'..='9' {
        digit_filter.push_str(&format!(" AND e.word NOT LIKE '%{d}%'"));
    }
    let rows = app
        .db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT DISTINCT e.word FROM dict_entries e \
                 JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
                 WHERE e.dictionary_id = $1 AND length(e.word) BETWEEN 2 AND 50{digit_filter} \
                   AND e.word_lower > $2 \
                 ORDER BY e.word_lower LIMIT $3"
            ),
            [dictionary_id.into(), query.after.clone().into(), limit.into()],
        ))
        .await
        .map_err(AppError::from)?;
    let words: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "word").ok())
        .collect();
    let next_cursor = if (words.len() as i64) < limit {
        None
    } else {
        words.last().map(|w| w.to_lowercase())
    };
    Ok(web::Json(json!({
        "dictionary_id": dictionary_id,
        "dictionary_name": dict.name,
        "words": words,
        "next_cursor": next_cursor,
    })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/tools")
            .route("/word-frequency", web::post().to(word_frequency))
            .route("/word-frequency-upload", web::post().to(word_frequency_upload))
            .route("/dict/browse/{dictionary_id}", web::get().to(browse)),
    );
}

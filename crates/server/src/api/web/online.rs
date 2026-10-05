//! GET /api/dict/online/lookup —— 移植自 `app/api/web/online.py`。

use actix_web::{web, HttpRequest};
use serde::Deserialize;

use crate::core::deps::get_web_caller;
use crate::core::errors::AppError;
use crate::services::online_dict_service;
use crate::services::web_rate_limit;
use crate::AppState;

#[derive(Deserialize)]
pub struct OnlineQuery {
    pub word: String,
    #[serde(default = "default_lang")]
    pub lang: String,
}

fn default_lang() -> String { "zh".to_string() }

pub async fn lookup(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    params: web::Query<OnlineQuery>,
) -> Result<web::Json<serde_json::Value>, AppError> {
    // 对齐 Python：限流在开关校验之前（开关查询自身很便宜，但保持了原顺序）
    let caller = get_web_caller(&req, &app).await?;
    web_rate_limit::enforce_online_rate(&app, caller.user.is_some(), &caller.ip).await?;
    if !valid_lang_tag(&params.lang) {
        return Err(AppError::validation("lang 参数无效"));
    }
    let result = online_dict_service::lookup_with_settings(&app, &app.cfg, &params.word, &params.lang).await?;
    Ok(web::Json(result))
}

/// lang 校验：对齐 Python 正则 `^[a-z]{2}(-[A-Za-z]{2,4})?$`（zh-Hans / en-US 合法）
fn valid_lang_tag(lang: &str) -> bool {
    let bytes = lang.as_bytes();
    if bytes.len() < 2 || !bytes[..2].iter().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    match lang.get(2..) {
        None => true,
        Some(rest) => match rest.strip_prefix('-') {
            // 子标签：2-4 个字母（大小写均可）
            Some(sub) => (2..=4).contains(&sub.len()) && sub.chars().all(|c| c.is_ascii_alphabetic()),
            None => false,
        },
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/dict/online/lookup", web::get().to(lookup));
}

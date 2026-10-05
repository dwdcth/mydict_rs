//! GET /api/dict/random —— 移植自 `app/api/web/random_pick.py`。
//!
//! 注意顺序（对齐 Python）：**功能开关在限流之前**——关着时不耗配额（与 online 相反）。

use actix_web::{web, HttpRequest};
use serde_json::json;

use crate::core::deps::get_web_caller;
use crate::core::errors::AppError;
use crate::services::{random_entry, settings_service, web_rate_limit};
use crate::AppState;

pub async fn random_pick(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<serde_json::Value>, AppError> {
    // 开关校验在限流之前：关闭时不消耗限流配额
    let enabled = settings_service::get_bool_setting(&app.db, "random_browse_enabled", false).await?;
    if !enabled {
        return Err(AppError::forbidden("随机浏览未开启"));
    }
    let caller = get_web_caller(&req, &app).await?;
    web_rate_limit::enforce_search_rate(
        &app,
        caller.user.is_some(),
        &caller.ip,
        caller.user.as_ref().map(|u| u.id),
    )
    .await?;

    let dict_ids = crate::services::query::parse_dict_ids(
        req.query_string()
            .split('&')
            .find_map(|kv| kv.strip_prefix("dict_ids=")),
    );
    let allowed = crate::core::deps::caller_allowed_ids(&app, caller.user.as_ref()).await?;
    let picked = random_entry::pick_random_entry(&app, allowed.as_deref(), dict_ids.as_deref()).await?;
    match picked {
        Some(value) => Ok(web::Json(value)),
        None => Err(AppError::not_found("没有可用的词典")),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/dict/random", web::get().to(random_pick));
}

#[allow(unused)]
fn _marker() {
    let _ = json!({});
}

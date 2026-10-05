//! /api/system/* —— 移植自 `app/api/system.py`

use actix_web::web;
use serde_json::json;

use crate::services::system_status;
use crate::AppState;

pub async fn info(app: web::Data<std::sync::Arc<AppState>>) -> web::Json<serde_json::Value> {
    web::Json(json!({
        "version": crate::core::version::get_app_version(&app.cfg.version_file_path),
    }))
}

pub async fn status(app: web::Data<std::sync::Arc<AppState>>) -> web::Json<serde_json::Value> {
    web::Json(system_status::get_status(&app))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/system/info", web::get().to(info))
        .route("/system/status", web::get().to(status));
}

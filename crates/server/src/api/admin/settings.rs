//! /api/admin/settings —— 移植自 `app/api/admin/settings.py`。

use actix_web::web;
use serde_json::Value;

use crate::core::deps::AdminAuth;
use crate::core::errors::AppError;
use crate::services::admin_settings_service;
use crate::AppState;

pub async fn get_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
) -> Result<web::Json<Value>, AppError> {
    Ok(web::Json(
        admin_settings_service::get_all_settings(&app.db, &app.cfg).await?,
    ))
}

/// 全字段可选；区分「未传」与「显式传 null」（fields_present 来自 body 的键集合）
pub async fn update_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
    body: web::Json<Value>,
) -> Result<web::Json<Value>, AppError> {
    let Some(obj) = body.as_object() else {
        return Err(AppError::validation("请求体必须是 JSON 对象"));
    };
    let fields_present: Vec<String> = obj.keys().cloned().collect();
    Ok(web::Json(
        admin_settings_service::update_settings(&app.db, obj, &fields_present, &app.cfg, _admin.0.id)
            .await?,
    ))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/settings", web::get().to(get_settings))
        .route("/admin/settings", web::put().to(update_settings));
}

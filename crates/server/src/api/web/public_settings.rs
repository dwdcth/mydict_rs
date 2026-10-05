//! GET /api/public/settings —— 移植自 `app/api/web/public_settings.py`。

use actix_web::web;
use serde_json::Value;

use crate::core::errors::AppError;
use crate::services::{admin_auth_service, admin_settings_service};
use crate::AppState;

pub async fn public_settings(app: web::Data<std::sync::Arc<AppState>>) -> Result<web::Json<Value>, AppError> {
    let initialized = admin_auth_service::is_initialized(&app.db).await?;
    Ok(web::Json(
        admin_settings_service::get_public_settings(&app.db, &app.cfg, initialized).await?,
    ))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/public/settings", web::get().to(public_settings));
}

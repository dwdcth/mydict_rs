//! /api/admin/tasks —— 移植自 `app/api/admin/tasks.py`。

use actix_web::web;
use serde_json::Value;

use crate::core::deps::AdminAuth;
use crate::core::errors::AppError;
use crate::AppState;

pub async fn running_tasks(
    _app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
) -> web::Json<Value> {
    web::Json(serde_json::Value::Array(_app.tasks.list_running()))
}

pub async fn get_task(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
    path: web::Path<i64>,
) -> Result<web::Json<Value>, AppError> {
    let task_id = path.into_inner();
    app.tasks
        .get(task_id)
        .map(web::Json)
        .ok_or_else(|| AppError::not_found("任务不存在或已过期"))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/tasks/running", web::get().to(running_tasks))
        .route("/admin/tasks/{task_id}", web::get().to(get_task));
}

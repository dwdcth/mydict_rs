//! 词典组 HTTP 端点（GoldenDict 式命名查询范围，Web 用户专属）。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::core::deps::UserAuth;
use crate::core::errors::AppError;
use crate::services::dict_group;
use crate::AppState;

#[derive(Deserialize)]
pub struct CreateGroupRequest {
    pub name: String,
    pub dictionary_ids: Vec<i32>,
}

#[derive(Deserialize)]
pub struct UpdateGroupRequest {
    pub name: Option<String>,
    pub dictionary_ids: Option<Vec<i32>>,
}

/// GET /api/dict/groups —— 当前用户的全部词典组
pub async fn list_groups(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let groups = dict_group::list_groups(&app.db, user.0.id).await?;
    Ok(web::Json(json!({ "groups": groups })))
}

/// POST /api/dict/groups —— 建组
pub async fn create_group(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<CreateGroupRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let group =
        dict_group::create_group(&app.db, user.0.id, &body.name, &body.dictionary_ids).await?;
    Ok(web::Json(group))
}

/// PUT /api/dict/groups/{id} —— 改名 / 全量替换成员
pub async fn update_group(
    app: web::Data<std::sync::Arc<AppState>>,
    path: web::Path<i32>,
    body: web::Json<UpdateGroupRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let group = dict_group::update_group(
        &app.db,
        user.0.id,
        path.into_inner(),
        body.name.as_deref(),
        body.dictionary_ids.as_deref(),
    )
    .await?;
    Ok(web::Json(group))
}

/// DELETE /api/dict/groups/{id}
pub async fn delete_group(
    app: web::Data<std::sync::Arc<AppState>>,
    path: web::Path<i32>,
    user: UserAuth,
) -> Result<HttpResponse, AppError> {
    dict_group::delete_group(&app.db, user.0.id, path.into_inner()).await?;
    Ok(HttpResponse::Ok().json(json!({"ok": true})))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/dict/groups")
            .route("", web::get().to(list_groups))
            .route("", web::post().to(create_group))
            .route("/{id}", web::put().to(update_group))
            .route("/{id}", web::delete().to(delete_group)),
    );
}

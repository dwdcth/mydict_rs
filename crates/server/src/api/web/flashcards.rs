//! 闪卡复习 HTTP 端点（FSRS 间隔重复，登录用户专属）。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::core::deps::UserAuth;
use crate::core::errors::AppError;
use crate::services::flashcards;
use crate::AppState;

#[derive(Deserialize)]
pub struct AddCardRequest {
    pub word: String,
    pub dictionary_id: Option<i32>,
}

#[derive(Deserialize)]
pub struct ReviewRequest {
    /// 1=Again 2=Hard 3=Good 4=Easy
    pub rating: i32,
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// due=只看到期+新卡（默认）；all=全部
    #[serde(default)]
    pub filter: String,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 {
    1
}
fn default_page_size() -> i64 {
    50
}

#[derive(Deserialize)]
pub struct QueueQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    flashcards::QUEUE_LIMIT
}

#[derive(Deserialize)]
pub struct SettingsRequest {
    /// 目标记忆率（0.7-0.99）
    pub retention: Option<f64>,
    /// 19 位 FSRS-4.5 权重 JSON 数组文本；空串=清空回默认；缺省=不动
    pub weights: Option<String>,
}

/// POST /api/flashcards —— 查询的词加入闪卡（词条不在生词本则先收藏）
pub async fn add_card(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    body: web::Json<AddCardRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let result = flashcards::add_card(&app, user.0.id, body.dictionary_id, &body.word).await?;
    Ok(web::Json(result))
}

/// GET /api/flashcards/queue —— 本次复习会话的到期队列（含四档预测间隔）
pub async fn queue(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    query: web::Query<QueueQuery>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let limit = query.limit.clamp(1, 100);
    Ok(web::Json(
        flashcards::review_queue(&app, user.0.id, limit).await?,
    ))
}

/// POST /api/flashcards/{vocab_item_id}/review —— 评分
pub async fn review(
    app: web::Data<std::sync::Arc<AppState>>,
    path: web::Path<i32>,
    body: web::Json<ReviewRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(
        flashcards::review_card(&app, user.0.id, path.into_inner(), body.rating).await?,
    ))
}

/// GET /api/flashcards —— 卡片列表 + 统计
pub async fn list(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    query: web::Query<ListQuery>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let filter = if query.filter == "all" { "all" } else { "due" };
    Ok(web::Json(
        flashcards::list_cards(&app, user.0.id, filter, query.page, query.page_size).await?,
    ))
}

/// GET /api/flashcards/stats —— 徽章/进度
pub async fn stats(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(flashcards::stats(&app, user.0.id).await?))
}

/// DELETE /api/flashcards/{vocab_item_id} —— 移出复习（生词本保留）
pub async fn delete(
    app: web::Data<std::sync::Arc<AppState>>,
    path: web::Path<i32>,
    user: UserAuth,
) -> Result<HttpResponse, AppError> {
    flashcards::delete_card(&app, user.0.id, path.into_inner()).await?;
    Ok(HttpResponse::Ok().json(json!({"ok": true})))
}

/// GET /api/flashcards/settings
pub async fn get_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(
        flashcards::get_settings(&app.db, user.0.id).await?,
    ))
}

/// PUT /api/flashcards/settings —— 调目标记忆率 / 导入自定义权重
pub async fn update_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    body: web::Json<SettingsRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(
        flashcards::update_settings(
            &app.db,
            user.0.id,
            body.retention,
            body.weights.as_deref(),
        )
        .await?,
    ))
}

#[derive(Deserialize)]
pub struct QuizQuery {
    #[serde(default = "default_quiz_count")]
    pub count: i64,
}

fn default_quiz_count() -> i64 {
    10
}

#[derive(Deserialize)]
pub struct QuizAnswerRequest {
    pub correct: bool,
}

/// GET /api/flashcards/quiz —— 例句挖空测验（到期卡优先；答对/答错映射 FSRS Good/Again）
pub async fn quiz(
    app: web::Data<std::sync::Arc<AppState>>,
    _req: HttpRequest,
    query: web::Query<QuizQuery>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    let count = query.count.clamp(1, 30);
    Ok(web::Json(
        crate::services::quiz::build_quiz(&app, user.0.id, count).await?,
    ))
}

/// POST /api/flashcards/quiz/{vocab_item_id}/answer —— 答对=Good、答错=Again
pub async fn quiz_answer(
    app: web::Data<std::sync::Arc<AppState>>,
    path: web::Path<i32>,
    body: web::Json<QuizAnswerRequest>,
    user: UserAuth,
) -> Result<web::Json<serde_json::Value>, AppError> {
    Ok(web::Json(
        crate::services::quiz::answer(&app, user.0.id, path.into_inner(), body.correct).await?,
    ))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/flashcards")
            .route("", web::get().to(list))
            .route("", web::post().to(add_card))
            .route("/queue", web::get().to(queue))
            .route("/quiz", web::get().to(quiz))
            .route("/quiz/{vocab_item_id}/answer", web::post().to(quiz_answer))
            .route("/stats", web::get().to(stats))
            .route("/settings", web::get().to(get_settings))
            .route("/settings", web::put().to(update_settings))
            .route("/{vocab_item_id}/review", web::post().to(review))
            .route("/{vocab_item_id}", web::delete().to(delete)),
    );
}

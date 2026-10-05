//! /api/admin/stats —— 移植自 `app/api/admin/stats.py`。

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::Value;

use crate::core::deps::require_admin;
use crate::core::errors::AppError;
use crate::services::stats_service;
use crate::AppState;

pub async fn overview(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
) -> Result<web::Json<Value>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(stats_service::get_overview(&app).await?))
}

#[derive(Deserialize)]
pub struct TopWordsQuery {
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 { 10 }

pub async fn top_words(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    query: web::Query<TopWordsQuery>,
) -> Result<web::Json<Vec<Value>>, AppError> {
    require_admin(&req, &app).await?;
    Ok(web::Json(
        stats_service::top_words(&app, query.start_date.as_deref(), query.end_date.as_deref(), query.limit.min(50)).await?,
    ))
}

#[derive(Deserialize)]
pub struct DimensionQuery {
    pub dimension: String,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub export: Option<String>,
}

pub async fn dimension(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    query: web::Query<DimensionQuery>,
) -> Result<HttpResponse, AppError> {
    require_admin(&req, &app).await?;
    let rows = stats_service::query_dimension_stats(
        &app,
        &query.dimension,
        query.start_date.as_deref(),
        query.end_date.as_deref(),
    )
    .await?;
    if query.export.as_deref() == Some("csv") {
        let csv = stats_service::stats_rows_to_csv(&rows);
        Ok(HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header(("Content-Disposition", format!("attachment; filename=\"stats-{}.csv\"", query.dimension)))
            .body(csv))
    } else {
        Ok(HttpResponse::Ok().json(Value::Array(rows)))
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/stats/overview", web::get().to(overview))
        .route("/admin/stats/top-words", web::get().to(top_words))
        .route("/admin/stats", web::get().to(dimension));
}

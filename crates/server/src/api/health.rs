//! GET /api/health —— 移植自 `app/api/health.py`

use actix_web::web;
use serde_json::json;

pub async fn health() -> web::Json<serde_json::Value> {
    web::Json(json!({"status": "ok"}))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health));
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as actix_test;

    #[actix_web::test]
    async fn returns_ok() {
        let app = actix_test::init_service(
            actix_web::App::new().configure(|cfg| {
                cfg.configure(configure);
            }),
        )
        .await;
        let req = actix_test::TestRequest::get().uri("/health").to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }
}

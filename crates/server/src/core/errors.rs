//! 业务异常与统一错误包络 —— 移植自 `app/core/exceptions.py`。
//!
//! 全局 handler 统一转换为 `{"code", "message", "detail"}` JSON；
//! 限流错误额外带 `Retry-After` 头。

use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub detail: Option<serde_json::Value>,
    pub retry_after: Option<i64>,
}

pub type ApiResult<T> = Result<T, AppError>;

impl AppError {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
            detail: None,
            retry_after: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<serde_json::Value>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "app_error", message)
    }
    pub fn invalid_credentials(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_credentials", message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub fn registration_disabled() -> Self {
        Self::new(StatusCode::FORBIDDEN, "registration_disabled", "当前不允许注册")
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }
    pub fn admin_already_initialized() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "admin_already_initialized",
            "管理员已初始化",
        )
    }
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation_error", message)
    }
    pub fn rate_limited(message: impl Into<String>, retry_after: i64) -> Self {
        let mut err = Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message);
        err.retry_after = Some(retry_after.max(1));
        err
    }
    /// DB 错误 → 500（不给调用方暴露内部细节，日志里记全量）
    /// 带用户可读消息的 500（TTS 未启用/引擎不可用等服务状态说明）
    pub fn internal_msg(message: &str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    pub fn internal(context: &str, source: impl std::fmt::Display) -> Self {
        tracing::error!(context, error = %source, "internal database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "服务器内部错误",
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        self.status
    }

    fn error_response(&self) -> HttpResponse {
        let body = json!({
            "code": self.code,
            "message": self.message,
            "detail": self.detail,
        });
        let mut resp = HttpResponse::build(self.status).json(body);
        if let Some(retry) = self.retry_after {
            resp.headers_mut().insert(
                actix_web::http::header::RETRY_AFTER,
                actix_web::http::header::HeaderValue::from_str(&retry.to_string())
                    .expect("numeric header"),
            );
        }
        resp
    }
}

impl From<sea_orm::DbErr> for AppError {
    fn from(err: sea_orm::DbErr) -> Self {
        AppError::internal("database", err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_matches_python() {
        let err = AppError::not_found("词条不存在");
        let resp = err.error_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        // code/message/detail 三键齐全
    }

    #[test]
    fn rate_limited_has_retry_after() {
        let err = AppError::rate_limited("请求过于频繁", 0);
        // max(1) 兜底
        assert_eq!(err.retry_after, Some(1));
    }
}

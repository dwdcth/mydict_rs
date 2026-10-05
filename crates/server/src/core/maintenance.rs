//! 维护门 —— 移植自 `app/core/maintenance.py`（MaintenanceGate ASGI 中间件）。
//!
//! 启动流程未就绪时，/api 下除健康检查与系统状态外一律 503。迁移期间 SQLite 被独占，
//! 放过任何业务请求都只会卡住或报锁错误。静态页面与 /dict-res 只读文件、不碰数据库，
//! 照常放行，前端才能加载出来展示维护提示。

use actix_web::body::BoxBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::Next;
use actix_web::{web, HttpResponse};
use serde_json::json;

use crate::AppState;

const MESSAGE: &str = "系统正在升级，请稍后再试";

fn blocked(path: &str) -> bool {
    if !path.starts_with("/api/") || path == "/api/health" {
        return false;
    }
    !path.starts_with("/api/system/")
}

pub async fn maintenance_gate(
    req: ServiceRequest,
    next: Next<BoxBody>,
) -> actix_web::Result<ServiceResponse<BoxBody>> {
    let is_blocked = req
        .app_data::<web::Data<std::sync::Arc<AppState>>>()
        .is_some_and(|state| !state.bootstrap.is_ready())
        && blocked(req.path());

    if is_blocked {
        let resp = HttpResponse::ServiceUnavailable()
            .insert_header(("Retry-After", "5"))
            .json(json!({
                "code": "maintenance",
                "message": MESSAGE,
                "detail": serde_json::Value::Null,
            }));
        return Ok(ServiceResponse::new(req.into_parts().0, resp));
    }
    next.call(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_api_but_not_health_or_static() {
        assert!(blocked("/api/admin/login"));
        assert!(blocked("/api/v1/query"));
        assert!(!blocked("/api/health"));
        assert!(!blocked("/api/system/status"));
        assert!(!blocked("/dict-res/1/res/a.css"));
        assert!(!blocked("/"));
        assert!(!blocked("/admin"));
    }
}

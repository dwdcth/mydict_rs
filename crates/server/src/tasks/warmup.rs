//! 启动期预热 —— 移植自 `app/core/bootstrap.py::_warm_random_bounds`。
//!
//! 随机浏览开着时，启动后在后台把词典主键区间算好（不阻塞就绪）。M4 随机词条
//! 服务落地时补实现。

use crate::AppState;

pub async fn warm_random_bounds(app: &std::sync::Arc<AppState>) {
    crate::services::random_entry::warm_bounds_in_background(app).await;
}

//! 给未登录访客看的系统状态 —— 移植自 `app/services/system_status_service.py`。
//!
//! 只读内存，不碰数据库（迁移期间数据库被独占）。公开任务（启动时的迁移）给出
//! 标题与进度；管理员发起的词典处理只给一句概括，不暴露词典名。

use serde_json::{json, Value};

use crate::core::bootstrap::Phase;
use crate::AppState;

const BUSY_NOTICE: &str = "后台正在处理词典数据，查询可能稍慢。";

pub fn get_status(app: &AppState) -> Value {
    let (phase, message) = app.bootstrap.snapshot();
    let busy = phase == Phase::Ready && app.tasks.any_non_public_running();
    json!({
        "phase": phase.as_str(),
        "blocking": phase != Phase::Ready,
        "message": message,
        "tasks": app.tasks.running_public_summary(),
        "busy_notice": if busy { Some(BUSY_NOTICE) } else { None },
    })
}

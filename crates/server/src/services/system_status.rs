//! 给未登录访客看的系统状态 —— 移植自 `app/services/system_status_service.py`。
//!
//! 只读内存，不碰数据库（迁移期间数据库被独占）。公开任务（启动时的迁移）给出
//! 标题与进度；管理员发起的词典处理只给一句概括，不暴露词典名。

use serde_json::{json, Value};

use crate::core::bootstrap::Phase;
use crate::AppState;

const BUSY_NOTICE: &str = "后台正在处理词典数据，查询可能稍慢。";

/// 进程 RSS（Linux 读 /proc/self/statm；其它平台 None）——观察词典句柄
/// 空闲回收效果用（不暴露词典名等敏感信息，纯数字）
fn process_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(resident_pages * 4096)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

pub fn get_status(app: &AppState) -> Value {
    let (phase, message) = app.bootstrap.snapshot();
    let busy = phase == Phase::Ready && app.tasks.any_non_public_running();
    json!({
        "phase": phase.as_str(),
        "blocking": phase != Phase::Ready,
        "message": message,
        "tasks": app.tasks.running_public_summary(),
        "busy_notice": if busy { Some(BUSY_NOTICE) } else { None },
        // 词典句柄缓存的常驻估算（按字节预算 LRU + 空闲回收的观察口）
        "memory": {
            "process_rss_bytes": process_rss_bytes(),
            "mdd_cache_bytes": app.mdd_resources.resident_bytes(),
            "definition_cache_bytes": app.definition_resources.resident_bytes(),
            "idle_unload_secs": app.cfg.dict_idle_unload_secs,
        },
    })
}

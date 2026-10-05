//! 进程内长任务登记表 —— 移植自 `app/services/background_task_service.py`。
//!
//! 运行中的任务供别的会话/标签页打开管理后台时也能看到"正在导入"的状态；
//! 发起方自己通过 get() 轮询 task_id 直到 success/error。不持久化，随进程重启清空。

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// 已结束（成功/失败）的任务保留多久供轮询方读取最终结果
const FINISHED_TTL_SECONDS: i64 = 600;

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SUCCESS: &str = "success";
pub const STATUS_ERROR: &str = "error";

pub struct TaskRecord {
    pub id: i64,
    pub task_type: String,
    pub title: String,
    /// 公开任务的标题与进度会经 /api/system/status 给未登录的访客看
    pub public: bool,
    pub status: &'static str,
    pub progress_data: Map<String, Value>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct BackgroundTasks {
    tasks: Mutex<HashMap<i64, TaskRecord>>,
    next_id: AtomicI64,
}

impl BackgroundTasks {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
        }
    }

    pub fn start(
        &self,
        task_type: impl Into<String>,
        title: impl Into<String>,
        public: bool,
    ) -> i64 {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        prune_finished(&mut tasks);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let now = Utc::now();
        tasks.insert(
            id,
            TaskRecord {
                id,
                task_type: task_type.into(),
                title: title.into(),
                public,
                status: STATUS_RUNNING,
                progress_data: Map::new(),
                result: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        id
    }

    /// 进度就是整个 progress_data dict 的覆盖写（对齐 Python 语义）
    pub fn update_progress(&self, task_id: i64, progress: Map<String, Value>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = tasks.get_mut(&task_id) {
            task.progress_data = progress;
            task.updated_at = Utc::now();
        }
    }

    pub fn succeed(&self, task_id: i64, result: Value) {
        self.finish(task_id, STATUS_SUCCESS, Some(result), None);
    }

    pub fn fail(&self, task_id: i64, error: impl Into<String>) {
        self.finish(task_id, STATUS_ERROR, None, Some(error.into()));
    }

    fn finish(
        &self,
        task_id: i64,
        status: &'static str,
        result: Option<Value>,
        error: Option<String>,
    ) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = tasks.get_mut(&task_id) {
            task.status = status;
            task.result = result;
            task.error = error;
            task.updated_at = Utc::now();
        }
    }

    pub fn get(&self, task_id: i64) -> Option<Value> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks.get(&task_id).map(task_to_json)
    }

    /// 运行中任务按创建时间排序
    pub fn list_running(&self) -> Vec<Value> {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let mut running: Vec<&TaskRecord> = tasks
            .values()
            .filter(|t| t.status == STATUS_RUNNING)
            .collect();
        running.sort_by_key(|t| t.created_at);
        running.into_iter().map(task_to_json).collect()
    }

    /// 供 /api/system/status 用：(public, elapsed_seconds, stage/done/total, title, id)
    pub fn running_public_summary(&self) -> Vec<Value> {
        self.list_running()
            .into_iter()
            .filter(|t| t["public"].as_bool().unwrap_or(false))
            .map(|t| {
                json!({
                    "id": t["id"],
                    "title": t["title"],
                    "stage": t["progress_data"].get("stage").cloned().unwrap_or(Value::Null),
                    "done": t["progress_data"].get("done").cloned().unwrap_or(Value::Null),
                    "total": t["progress_data"].get("total").cloned().unwrap_or(Value::Null),
                    "elapsed_seconds": t["elapsed_seconds"],
                })
            })
            .collect()
    }

    pub fn any_non_public_running(&self) -> bool {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        tasks
            .values()
            .any(|t| t.status == STATUS_RUNNING && !t.public)
    }
}

fn task_to_json(task: &TaskRecord) -> Value {
    let elapsed = (Utc::now() - task.created_at).num_seconds().max(0);
    json!({
        "id": task.id,
        "task_type": task.task_type,
        "title": task.title,
        "public": task.public,
        "status": task.status,
        "progress_data": Value::Object(task.progress_data.clone()),
        "result": task.result.clone().unwrap_or(Value::Null),
        "error": task.error.clone().map(Value::String).unwrap_or(Value::Null),
        // 进程内内存值（非 DB），对齐 Python aware-datetime 序列化为带 Z 的 ISO
        "created_at": task.created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "updated_at": task.updated_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "elapsed_seconds": elapsed,
    })
}

fn prune_finished(tasks: &mut HashMap<i64, TaskRecord>) {
    let now = Utc::now();
    tasks.retain(|_, task| {
        task.status == STATUS_RUNNING
            || (now - task.updated_at).num_seconds() <= FINISHED_TTL_SECONDS
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_start_progress_succeed() {
        let svc = BackgroundTasks::new();
        let id = svc.start("dictionary_import", "导入词典", false);
        assert_eq!(svc.get(id).unwrap()["status"], "running");

        let mut progress = Map::new();
        progress.insert("done".into(), json!(5));
        progress.insert("total".into(), json!(10));
        svc.update_progress(id, progress);
        assert_eq!(svc.get(id).unwrap()["progress_data"]["done"], 5);

        svc.succeed(id, json!({"imported": 10}));
        let done = svc.get(id).unwrap();
        assert_eq!(done["status"], "success");
        assert_eq!(done["result"]["imported"], 10);
    }

    #[test]
    fn finished_tasks_expire_only_after_ttl() {
        let svc = BackgroundTasks::new();
        let id = svc.start("t", "t", false);
        svc.fail(id, "boom");
        assert!(svc.get(id).is_some());
        // 新任务启动会触发清理，但 TTL 内仍在
        let id2 = svc.start("t2", "t2", false);
        assert!(svc.get(id).is_some());
        let _ = id2;
    }

    #[test]
    fn ids_are_increasing() {
        let svc = BackgroundTasks::new();
        let a = svc.start("a", "a", false);
        let b = svc.start("b", "b", false);
        assert_eq!(b, a + 1);
    }
}

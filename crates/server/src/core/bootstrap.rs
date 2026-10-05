//! 启动流程 —— 移植自 `app/core/bootstrap.py`。
//!
//! HTTP 服务先起来，数据库迁移等耗时处理放到后台任务。迁移在大库上可能要几十分钟，
//! 若在监听端口之前同步执行，期间页面根本打不开、看不到任何提示。现在服务一启动就能
//! 响应 /api/system/status 与静态页面，业务接口在就绪前由 maintenance 中间件统一 503。

use std::sync::RwLock;

use migration::Migrator;
use sea_orm_migration::MigratorTrait;

use crate::services::background_task::BackgroundTasks;
use crate::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Migrating,
    Failed,
    Ready,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Migrating => "migrating",
            Phase::Failed => "failed",
            Phase::Ready => "ready",
        }
    }
}

pub const MIGRATING_MESSAGE: &str = "正在升级数据库，稍候即可恢复服务。";
pub const MIGRATING_HEAVY_MESSAGE: &str =
    "正在升级数据库，其中包含整表重建，大型词库可能需要几十分钟，期间暂停服务。";
pub const FAILED_MESSAGE: &str =
    "数据库升级失败，请管理员查看服务日志后重启服务，升级会从中断处继续。";

/// 启动状态机（对应 Python 模块级 _phase/_message + 锁）
pub struct BootstrapState {
    inner: RwLock<(Phase, Option<String>)>,
}

impl BootstrapState {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new((Phase::Starting, None)),
        }
    }

    pub fn set(&self, phase: Phase, message: Option<String>) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = (phase, message);
    }

    pub fn snapshot(&self) -> (Phase, Option<String>) {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_ready(&self) -> bool {
        self.snapshot().0 == Phase::Ready
    }
}

/// 执行启动流程直到就绪或失败。失败时进程不退出，停在 FAILED 让页面能展示原因。
pub async fn run(app: &std::sync::Arc<AppState>) {
    if let Err(err) = run_inner(app).await {
        tracing::error!(error = %err, "数据库升级失败");
        app.bootstrap.set(Phase::Failed, Some(FAILED_MESSAGE.to_string()));
    }
}

async fn run_inner(app: &std::sync::Arc<AppState>) -> Result<(), sea_orm::DbErr> {
    let pending = Migrator::get_applied_migrations(&app.db)
        .await
        .map(|applied| {
            let total = Migrator::migrations().len() as u64;
            total.saturating_sub(applied.len() as u64)
        })
        .unwrap_or(Migrator::migrations().len() as u64);

    if pending > 0 {
        migrate(app, pending).await?;
    }

    if app.cfg.enable_scheduler {
        crate::tasks::scheduler::start(app);
    }
    crate::tasks::warmup::warm_random_bounds(app).await;

    app.bootstrap.set(Phase::Ready, None);
    tracing::info!(version = crate::core::version::get_app_version(&app.cfg.version_file_path), "mydict 就绪");
    Ok(())
}

async fn migrate(app: &AppState, pending: u64) -> Result<(), sea_orm::DbErr> {
    // 当前迁移集没有 heavy 项（全新 schema 一步建表）；常量与分支保留，
    // 未来出现整表重建类迁移时按 Python 版挂 heavy 标记即可。
    app.bootstrap
        .set(Phase::Migrating, Some(MIGRATING_MESSAGE.to_string()));
    let task_id = app.tasks.start("system_migration", "升级数据库", true);
    report_progress(&app.tasks, task_id, 0, pending, "开始");

    // 逐个执行以便上报进度（Migrator::up(Some(1)) 每次推进一条）
    for done in 0..pending {
        report_progress(&app.tasks, task_id, done, pending, "数据库结构");
        Migrator::up(&app.db, Some(1)).await?;
    }
    app.tasks.succeed(
        task_id,
        serde_json::json!({"total": pending}),
    );
    Ok(())
}

fn report_progress(tasks: &BackgroundTasks, task_id: i64, done: u64, total: u64, stage: &str) {
    let mut progress = serde_json::Map::new();
    progress.insert("done".into(), done.into());
    progress.insert("total".into(), total.into());
    progress.insert("stage".into(), stage.into());
    tasks.update_progress(task_id, progress);
}

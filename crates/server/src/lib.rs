//! 应用状态与装配。

use std::collections::HashSet;
use std::sync::Arc;

use sea_orm::DatabaseConnection;

pub mod api;
pub mod core;
pub mod dictres;
pub mod entities;
pub mod services;
pub mod static_files;
pub mod tasks;

use crate::core::bootstrap::BootstrapState;
use crate::core::query_cache::QueryCache;
use crate::core::rate_limiter::MinuteCounters;
use crate::services::background_task::BackgroundTasks;
use crate::services::mdd_resources::MddResources;
use crate::services::mdx_resources::DefinitionResources;
use crate::services::tts::TtsState;
use crate::services::random_entry::RandomBounds;

/// 进程内全局状态（单 worker 下等价于 Python 的模块级全局变量集合）。
/// 随里程碑推进追加：查询缓存、限流计数器、随机区间缓存、opencc 实例等。
pub struct AppState {
    pub cfg: Arc<core::config::Settings>,
    pub db: DatabaseConnection,
    pub bootstrap: BootstrapState,
    pub tasks: BackgroundTasks,
    /// 查询结果缓存（300s/64MB 按体积计重，词典任何元数据/状态变更全量失效）
    pub query_cache: QueryCache,
    /// 随机浏览主键区间缓存（3600s）
    pub random_bounds: RandomBounds,
    /// 每分钟限流计数器（固定墙钟分钟窗，70s TTL）
    pub minute_counters: MinuteCounters,
    /// .mdd 直接读取（字节预算句柄缓存 + 256MB 资源字节缓存，均带空闲回收）
    pub mdd_resources: MddResources,
    /// lite 词典释义物化（字节预算句柄缓存 + 64MB 释义缓存，均带空闲回收）
    pub definition_resources: DefinitionResources,
    /// TTS（kokoro-micro 内嵌引擎，懒加载 + 结果缓存）
    pub tts: TtsState,
    /// 导入/重解析/修复/VACUUM 等大写库任务互斥（SQLite 单写者，排队而非失败）
    pub bulk_write: tokio::sync::Mutex<()>,
    /// 正在重新解析的词典 id（同词典并发重解析拒绝）
    pub reparsing: std::sync::Mutex<HashSet<i32>>,
    /// VACUUM 排队去重标志
    pub vacuum_pending: std::sync::atomic::AtomicBool,
}

impl AppState {
    pub fn new(cfg: core::config::Settings, db: DatabaseConnection) -> Self {
        // 空闲回收下限 60s：更小的值等于每次查询都重开词典（词头索引构建秒级起）
        let idle = std::time::Duration::from_secs(cfg.dict_idle_unload_secs.max(60));
        Self {
            mdd_resources: MddResources::new(
                cfg.dict_mdd_handle_budget_mb * 1024 * 1024,
                idle,
            ),
            definition_resources: DefinitionResources::new(
                cfg.dict_mdx_handle_budget_mb * 1024 * 1024,
                idle,
            ),
            cfg: Arc::new(cfg),
            db,
            bootstrap: BootstrapState::new(),
            tasks: BackgroundTasks::new(),
            query_cache: QueryCache::new(),
            random_bounds: RandomBounds::new(),
            minute_counters: MinuteCounters::new(),
            tts: TtsState::new(),
            bulk_write: tokio::sync::Mutex::new(()),
            reparsing: std::sync::Mutex::new(HashSet::new()),
            vacuum_pending: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 空闲内存回收（调度器周期调用）：
    /// 1. 各 moka 缓存跑一次维护任务——空闲超时（DICT_IDLE_UNLOAD_SECS）的词典
    ///    句柄/资源真正从缓存移除、Arc 释放（大数组走 munmap 归还 OS）
    /// 2. glibc malloc_trim 把释放回 arena 的空页归还内核（HashMap 小块等）
    /// 对齐 MDict/GoldenDict 的「关闭长期未用的词典」：服务端无手势，靠定时器兜底
    pub fn release_idle_memory(&self) {
        self.query_cache.run_maintenance();
        self.mdd_resources.run_maintenance();
        self.definition_resources.run_maintenance();
        self.tts.run_maintenance();
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// 路由装配（集成测试与 main 共用）。注册顺序对齐 Python main.py。
/// state 以 Arc 注册：后台任务（导入/重解析）需要克隆整个状态。
pub fn configure_app(cfg: &mut actix_web::web::ServiceConfig, state: Arc<AppState>) {
    cfg.app_data(actix_web::web::Data::new(state))
        .service(
            actix_web::web::scope("/api")
                .configure(api::health::configure)
                .configure(api::system::configure)
                .configure(api::admin::auth::configure)
                .configure(api::web::auth::configure)
                .configure(api::admin::dictionaries::configure)
                .configure(api::admin::tokens::configure)
                .configure(api::admin::users::configure)
                .configure(api::admin::settings::configure)
                .configure(api::admin::stats::configure)
                .configure(api::admin::tasks::configure)
                .configure(api::v1::query::configure)
                .configure(api::v1::vocab::configure)
                .configure(api::web::dict::configure)
                .configure(api::web::groups::configure)
                .configure(api::web::flashcards::configure)
                .configure(api::web::tools::configure)
                .configure(api::web::online::configure)
                .configure(api::web::random_pick::configure)
                .configure(api::web::vocab::configure)
                .configure(api::web::public_settings::configure),
        )
        .configure(dictres::configure)
        .configure(static_files::configure);
}

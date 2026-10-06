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
use crate::services::random_entry::RandomBounds;

/// 进程内全局状态（单 worker 下等价于 Python 的模块级全局变量集合）。
/// 随里程碑推进追加：查询缓存、限流计数器、随机区间缓存、opencc 实例等。
pub struct AppState {
    pub cfg: Arc<core::config::Settings>,
    pub db: DatabaseConnection,
    pub bootstrap: BootstrapState,
    pub tasks: BackgroundTasks,
    /// 查询结果缓存（300s/10k，词典任何元数据/状态变更全量失效）
    pub query_cache: QueryCache,
    /// 随机浏览主键区间缓存（3600s）
    pub random_bounds: RandomBounds,
    /// 每分钟限流计数器（固定墙钟分钟窗，70s TTL）
    pub minute_counters: MinuteCounters,
    /// .mdd 直接读取（句柄缓存 + 256MB 资源字节缓存）——磁盘优化的核心
    pub mdd_resources: MddResources,
    /// lite 词典释义物化（解析器句柄缓存 + 64MB 释义缓存）
    pub definition_resources: DefinitionResources,
    /// 导入/重解析/修复/VACUUM 等大写库任务互斥（SQLite 单写者，排队而非失败）
    pub bulk_write: tokio::sync::Mutex<()>,
    /// 正在重新解析的词典 id（同词典并发重解析拒绝）
    pub reparsing: std::sync::Mutex<HashSet<i32>>,
    /// VACUUM 排队去重标志
    pub vacuum_pending: std::sync::atomic::AtomicBool,
}

impl AppState {
    pub fn new(cfg: core::config::Settings, db: DatabaseConnection) -> Self {
        Self {
            cfg: Arc::new(cfg),
            db,
            bootstrap: BootstrapState::new(),
            tasks: BackgroundTasks::new(),
            query_cache: QueryCache::new(),
            random_bounds: RandomBounds::new(),
            minute_counters: MinuteCounters::new(),
            mdd_resources: MddResources::new(),
            definition_resources: DefinitionResources::new(),
            bulk_write: tokio::sync::Mutex::new(()),
            reparsing: std::sync::Mutex::new(HashSet::new()),
            vacuum_pending: std::sync::atomic::AtomicBool::new(false),
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
                .configure(api::web::online::configure)
                .configure(api::web::random_pick::configure)
                .configure(api::web::vocab::configure)
                .configure(api::web::public_settings::configure),
        )
        .configure(dictres::configure)
        .configure(static_files::configure);
}

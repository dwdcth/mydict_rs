//! 数据库连接 —— 移植自 `app/core/db.py`。
//!
//! Python 版每连接 PRAGMA：`foreign_keys=ON`、`journal_mode=WAL`、`busy_timeout=5000`。
//! SeaORM 下通过 `map_sqlx_sqlite_opts` 在建池前改写 sqlx 的 SqliteConnectOptions
//! （sqlx 的 URL 查询参数只支持 mode/cache 等少数几个，PRAGMA 必须走 builder）。
//!
//! - `create_if_missing`：对齐 SQLAlchemy create_engine + 首次建文件
//! - `foreign_keys(true)`：SQLite 默认不强制外键，ON DELETE CASCADE 需要显式开启
//! - `journal_mode(Wal)`：读不阻塞写
//! - `busy_timeout(5s)`：写冲突等待重试而不是立刻报错
//! - `synchronous(Normal)`：有意偏差 —— Python 版未设置（默认 FULL）；WAL + NORMAL
//!   不损坏数据、断电至多丢最后一个事务，写吞吐高 2-5 倍（大词库导入受益）。
//!   如需严格对齐改回 Full 即可。

use std::time::Duration;

use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

use crate::core::config::Settings;

pub async fn connect(settings: &Settings) -> Result<DatabaseConnection, sea_orm::DbErr> {
    let url = settings.database_url();
    let mut opts = ConnectOptions::new(&url);
    // SQLite 单写者：大写任务经 bulk_write 互斥排队，查询本身毫秒级——
    // 4 个连接足够（每连接还有 ~2MB 页缓存常驻，池越大 RSS 越高）
    opts.max_connections(4).min_connections(1).sqlx_logging(false);
    if settings.is_sqlite() {
        opts.map_sqlx_sqlite_opts(sqlite_options);
    }
    Database::connect(opts).await
}

/// 独立裸连接（VACUUM / wal_checkpoint 用 —— 不能在池的事务上执行）
pub async fn raw_connection(settings: &Settings) -> Result<DatabaseConnection, sea_orm::DbErr> {
    let url = settings.database_url();
    let mut opts = ConnectOptions::new(&url);
    opts.max_connections(1).sqlx_logging(false);
    if settings.is_sqlite() {
        opts.map_sqlx_sqlite_opts(sqlite_options);
    }
    Database::connect(opts).await
}

fn sqlite_options(base: SqliteConnectOptions) -> SqliteConnectOptions {
    // base 已由 sea-orm 从 URL 解析（filename 等），这里只叠加 PRAGMA
    base.create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_millis(5000))
        .foreign_keys(true)
}

/// 按 URL 字符串建独立裸连接（VACUUM 用；后台线程里没有现成的 AppState）
pub async fn raw_connection_url(
    url: &str,
    is_sqlite: bool,
) -> Result<DatabaseConnection, sea_orm::DbErr> {
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(1).sqlx_logging(false);
    if is_sqlite {
        opts.map_sqlx_sqlite_opts(sqlite_options);
    }
    Database::connect(opts).await
}

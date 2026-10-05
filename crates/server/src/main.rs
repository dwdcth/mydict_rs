//! MyDict 词典服务（Rust 重写版）。
//!
//! 启动顺序对齐 Python 版：先起 HTTP（维护门挡业务请求），后台任务跑迁移 →
//! 定时任务 → 预热 → 就绪。

use actix_web::{middleware, App, HttpServer};
use std::sync::Arc;

use server::core::{config::Settings, db, logging};
use server::{configure_app, AppState};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let settings = Settings::from_env();
    settings.ensure_data_dirs()?;
    logging::configure_logging(&settings.log_dir);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8000".to_string());

    let database = match db::connect(&settings).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("数据库连接失败（{}）: {err}", settings.database_url());
            std::process::exit(1);
        }
    };

    let state = Arc::new(AppState::new(settings, database));
    let bootstrap_state = state.clone();

    // 后台执行启动流程（迁移等），HTTP 立即可服务
    actix_web::rt::spawn(async move {
        server::core::bootstrap::run(&bootstrap_state).await;
    });

    let local_state = state.clone();
    HttpServer::new(move || {
        App::new()
            .wrap(middleware::from_fn(server::core::maintenance::maintenance_gate))
            .configure(|cfg| configure_app(cfg, local_state.clone()))
    })
    // 单 worker：进程内缓存（限流计数/查询缓存/后台任务表）与 Python 单进程部署对等
    .workers(1)
    .bind(&bind_addr)?
    .run()
    .await
}

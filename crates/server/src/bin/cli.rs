//! mydict-cli —— 移植自 `app/cli.py` 的 4 个命令。
//!
//! reset-admin-password / repair-entry-links / redetect-languages / migrate

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mydict-cli", about = "MyDict 管理命令行工具")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 重置管理员密码
    ResetAdminPassword {
        #[arg(long, default_value = "admin")]
        username: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// 就地修复历史坏链接（entry:/ sound:/ file:/ 前缀）
    RepairEntryLinks {
        #[arg(long)]
        dictionary_id: i32,
        #[arg(long, default_value = "5000")]
        batch_size: i64,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// 按当前采样逻辑重新识别词典语言方向
    RedetectLanguages {
        #[arg(long)]
        dictionary_id: Option<i32>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
    /// 手动执行数据库迁移（先停服务）
    Migrate {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        yes: bool,
    },
}

fn read_password(prompt: &str) -> String {
    eprint!("{prompt}");
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .ok();
    buf.trim().to_string()
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("错误: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let settings = server::core::config::Settings::from_env();
    let _ = settings.ensure_data_dirs();
    server::core::logging::configure_logging(&settings.log_dir);
    let db = server::core::db::connect(&settings)
        .await
        .map_err(|e| anyhow::anyhow!("数据库连接失败: {e}"))?;
    let state = std::sync::Arc::new(server::AppState::new(settings.clone(), db));

    let result: anyhow::Result<()> = match cli.command {
        Commands::ResetAdminPassword { username, password } => {
            let password = password.unwrap_or_else(|| {
                let first = read_password("新密码（8-128 字符）: ");
                if first.len() < 8 {
                    eprintln!("密码过短");
                    std::process::exit(1);
                }
                let second = read_password("再输一遍: ");
                if first != second {
                    eprintln!("两次输入不一致");
                    std::process::exit(1);
                }
                first
            });
            let hash = match server::core::security::hash_password(&password) {
                Ok(hash) => hash,
                Err(err) => {
                    eprintln!("哈希失败: {err}");
                    anyhow::bail!("失败");
                }
            };
            use sea_orm::ConnectionTrait;
            let result = state
                .db
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    state.db.get_database_backend(),
                    "UPDATE admins SET password_hash = $1 WHERE username = $2",
                    [hash.into(), username.clone().into()],
                ))
                .await;
            match result {
                Ok(res) if res.rows_affected() > 0 => {
                    println!("管理员 {username} 的密码已重置");
                    Ok(())
                }
                Ok(_) => anyhow::bail!("管理员 {username} 不存在"),
                Err(err) => anyhow::bail!("失败: {err}"),
            }
        }
        Commands::RepairEntryLinks { dictionary_id, batch_size, dry_run, yes } => {
            let repaired = match server::services::definition_repair::repair_legacy_links(
                &state, dictionary_id, batch_size, dry_run,
            )
            .await
            {
                Ok(n) => n,
                Err(err) => {
                    eprintln!("修复失败: {}", err.message);
                    anyhow::bail!("失败");
                }
            };
            if dry_run {
                println!("dry-run：待修复约 {repaired} 行（未写入）");
                Ok(())
            } else if yes || {
                eprint!("将修复词典 {dictionary_id} 的坏链接，确认执行？[y/N] ");
                let mut buf = String::new();
                std::io::stdin().read_line(&mut buf).ok();
                buf.trim().eq_ignore_ascii_case("y")
            } {
                println!("修复完成，改动 {repaired} 行");
                println!("提示：容器内查询缓存清不到，建议重启容器");
                Ok(())
            } else {
                println!("已取消");
                Ok(())
            }
        }
        Commands::RedetectLanguages { dictionary_id, dry_run, yes } => {
            let ids: Vec<i32> = match dictionary_id {
                Some(id) => vec![id],
                None => server::services::dictionary::list_dictionaries(&state.db)
                    .await
                    .map(|dicts| dicts.iter().map(|d| d.id).collect())
                    .unwrap_or_default(),
            };
            for id in ids {
                let (from, to) =
                    match server::services::dictionary::detect_dictionary_language(&state.db, id).await {
                        Ok(pair) => pair,
                        Err(err) => {
                            eprintln!("词典 {id}: {}", err.message);
                            continue;
                        }
                    };
                let name = server::services::dictionary::list_dictionaries(&state.db)
                    .await
                    .ok()
                    .and_then(|dicts| dicts.into_iter().find(|d| d.id == id))
                    .map(|d| d.name)
                    .unwrap_or_else(|| id.to_string());
                if dry_run {
                    println!("[dry-run] {name}: lang_from={:?} lang_to={:?}", from, to);
                    continue;
                }
                if yes || {
                    eprint!("把「{name}」的语言方向改为 {from:?}/{to:?}？[y/N] ");
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf).ok();
                    buf.trim().eq_ignore_ascii_case("y")
                } {
                    server::services::dictionary::apply_detected_language(
                        &state, id, from.as_deref(), to.as_deref(),
                    )
                    .await
                    .ok();
                    println!("已更新 {name}");
                }
            }
            Ok(())
        }
        Commands::Migrate { dry_run, yes } => {
            use sea_orm_migration::MigratorTrait;
            let applied = migration::Migrator::get_applied_migrations(&state.db).await;
            let total = migration::Migrator::migrations().len();
            match applied {
                Ok(applied) => {
                    let pending = total.saturating_sub(applied.len());
                    if pending == 0 {
                        println!("数据库已是最新（{total} 个迁移全部应用）");
                        return Ok(());
                    }
                    println!("待执行迁移：{pending} 个");
                    if dry_run {
                        return Ok(());
                    }
                    if !yes {
                        eprint!("执行迁移？[y/N] ");
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf).ok();
                        if !buf.trim().eq_ignore_ascii_case("y") {
                            println!("已取消");
                            return Ok(());
                        }
                    }
                    match migration::Migrator::up(&state.db, None).await {
                        Ok(_) => {
                            println!("迁移完成");
                            Ok(())
                        }
                        Err(err) => anyhow::bail!("迁移失败: {err}"),
                    }
                }
                Err(err) => anyhow::bail!("读取迁移状态失败: {err}"),
            }
        }
    };
    let _ = result;
    Ok(())
}

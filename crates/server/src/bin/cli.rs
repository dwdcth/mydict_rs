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
    /// 就地修复历史坏链接（entry:/ sound:/ file:/ 前缀）；省略 --dictionary-id = 全部词典
    RepairEntryLinks {
        #[arg(long)]
        dictionary_id: Option<i32>,
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
                    println!("管理员 '{username}' 密码已重置");
                    Ok(())
                }
                Ok(_) => anyhow::bail!("管理员 {username} 不存在"),
                Err(err) => anyhow::bail!("失败: {err}"),
            }
        }
        Commands::RepairEntryLinks { dictionary_id, batch_size, dry_run, yes } => {
            // 对齐 Python：无 --dry-run 且无 --yes 一律拒绝（绝不交互确认——脚本/容器里
            // stdin 关着，交互提示等于卡死）
            if !dry_run && !yes {
                eprintln!(
                    "该操作会就地改写 dict_entries.definition，影响面可能达数百万行。\n请先用 --dry-run 查看待修数量；确认无误后加 --yes 执行。"
                );
                return Err(anyhow::anyhow!("需要 --yes"));
            }
            let targets: Vec<(i32, String)> = match dictionary_id {
                Some(id) => {
                    let name = server::services::dictionary::list_dictionaries(&state.db)
                        .await
                        .ok()
                        .and_then(|dicts| dicts.into_iter().find(|d| d.id == id))
                        .map(|d| d.name)
                        .unwrap_or_else(|| id.to_string());
                    vec![(id, name)]
                }
                None => server::services::dictionary::list_dictionaries(&state.db)
                    .await
                    .map(|dicts| dicts.into_iter().map(|d| (d.id, d.name)).collect())
                    .unwrap_or_default(),
            };
            println!(
                "{}待处理词典 {} 部",
                if dry_run { "[dry-run] " } else { "" },
                targets.len()
            );
            let mut total = 0i64;
            let mut touched = 0i64;
            for (id, name) in &targets {
                // 先统计：dry-run 用，也避免为没有坏链的词典白跑一遍替换
                let (entry_rows, sound_rows, file_rows) = match server::services::definition_repair::count_legacy_links(&state, *id).await {
                    Ok(c) => c,
                    Err(err) => {
                        eprintln!("  id={id} {name}：跳过（{}）", err.message);
                        continue;
                    }
                };
                if entry_rows == 0 && sound_rows == 0 && file_rows == 0 {
                    continue;
                }
                touched += 1;
                println!("  id={id} {name}：entry {entry_rows} 行 / sound {sound_rows} 行 / file {file_rows} 行");
                if dry_run {
                    total += entry_rows + sound_rows + file_rows;
                    continue;
                }
                let started = std::time::Instant::now();
                match server::services::definition_repair::repair_legacy_links(
                    &state, *id, batch_size, false,
                )
                .await
                {
                    Ok(repaired) => {
                        total += repaired;
                        println!("      已修复 {repaired} 行，用时 {:.1}s", started.elapsed().as_secs_f64());
                    }
                    Err(err) => eprintln!("      失败：{}", err.message),
                }
            }
            if touched == 0 {
                println!("没有找到需要修复的坏链接。");
            } else {
                println!(
                    "{} {total} 行（{touched} 部词典）",
                    if dry_run { "合计待修复" } else { "合计修复" }
                );
            }
            if !dry_run && touched > 0 {
                println!("\n提示：查询结果缓存在应用进程内，本次修复不会清掉它。请重启应用容器后再验证。");
            }
            Ok(())
        }
        Commands::RedetectLanguages { dictionary_id, dry_run, yes } => {
            if !dry_run && !yes {
                eprintln!(
                    "该操作会改写 dictionaries.lang_from / lang_to，可能覆盖手工修正过的值。\n请先用 --dry-run 查看识别结果；确认无误后加 --yes 执行。"
                );
                return Err(anyhow::anyhow!("需要 --yes"));
            }
            let targets: Vec<server::entities::dictionary::Model> = match dictionary_id {
                Some(id) => server::services::dictionary::list_dictionaries(&state.db)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|d| d.id == id)
                    .collect(),
                None => server::services::dictionary::list_dictionaries(&state.db)
                    .await
                    .unwrap_or_default(),
            };
            println!(
                "{}待识别词典 {} 部",
                if dry_run { "[dry-run] " } else { "" },
                targets.len()
            );
            let mut changed = 0i64;
            for dict in &targets {
                let before = format!("{}→{}", dict.lang_from, dict.lang_to);
                let (from, to) = match server::services::dictionary::detect_dictionary_language(&state.db, dict.id).await {
                    Ok(pair) => pair,
                    Err(err) => {
                        println!("  id={} {}：跳过（{}）", dict.id, dict.name, err.message);
                        continue;
                    }
                };
                let after = format!(
                    "{}→{}",
                    from.as_deref().unwrap_or(&dict.lang_from),
                    to.as_deref().unwrap_or(&dict.lang_to)
                );
                let mark = if after != before { "← 与现值不同" } else { "" };
                println!("  id={} {}：{before} → {after} {mark}", dict.id, dict.name);
                if dry_run || after == before {
                    continue;
                }
                if server::services::dictionary::apply_detected_language(
                    &state, dict.id, from.as_deref(), to.as_deref(),
                )
                .await
                .is_ok()
                {
                    changed += 1;
                }
            }
            if dry_run {
                println!("\ndry-run 未写入任何改动。");
            } else {
                println!("\n共更新 {changed} 部词典的语言方向。");
                if changed > 0 {
                    println!("提示：查询结果缓存在应用进程内，请重启应用容器后再验证。");
                }
            }
            Ok(())
        }
        Commands::Migrate { dry_run, yes } => {
            use sea_orm_migration::MigratorTrait;
            let applied = migration::Migrator::get_applied_migrations(&state.db).await;
            match applied {
                Ok(applied) => {
                    let applied_names: Vec<String> =
                        applied.iter().map(|m| m.name().to_string()).collect();
                    let pending: Vec<String> = migration::Migrator::migrations()
                        .iter()
                        .map(|m| m.name().to_string())
                        .filter(|name| !applied_names.contains(name))
                        .collect();
                    if pending.is_empty() {
                        println!("数据库已是最新版本，无需迁移");
                        return Ok(());
                    }
                    println!("待执行的迁移 {} 个：", pending.len());
                    for name in &pending {
                        println!("  {name}");
                    }
                    if dry_run {
                        return Ok(());
                    }
                    if !yes {
                        eprintln!(
                            "请先停止服务、备份数据库文件，确认后加 --yes 执行。中途被打断可以直接重跑。"
                        );
                        return Err(anyhow::anyhow!("需要 --yes"));
                    }
                    let started = std::time::Instant::now();
                    match migration::Migrator::up(&state.db, None).await {
                        Ok(_) => {
                            println!("迁移完成，用时 {:.0} 秒", started.elapsed().as_secs_f64());
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

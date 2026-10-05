//! 词典管理：导入 / 重解析（代切换）/ dicts_dir 扫描分组 / CRUD / 删除
//! —— 移植自 `app/services/dictionary_service.py`。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde_json::{json, Map, Value};

use dict_parser::resources::SIBLING_RESOURCE_EXTENSIONS;
use dict_parser::{parser_by_format, ParseOpts};

use crate::core::config::Settings;
use crate::core::errors::AppError;
use crate::services::audit_service;
use crate::services::language_detect::{
    detect_language, FALLBACK_LANG_FROM, FALLBACK_LANG_TO,
};
use crate::AppState;

/// 批量插入每批条数（对齐 Python BATCH_SIZE）
pub const BATCH_SIZE: usize = 2000;
/// 重新解析删除上一代词条时每批的行数：每批一个短事务，不长占写锁
const PURGE_BATCH_SIZE: i64 = 20_000;
/// 语言识别采样条数（词头可均匀取样所以多取；释义顺序读 200 条够判断）
const SAMPLE_HEADWORD_LIMIT: usize = 500;
const SAMPLE_DEFINITION_LIMIT: usize = 200;
/// 递归扫描的最大深度（相对扫描起点）
const MAX_SCAN_DEPTH: usize = 4;

pub const VALID_FORMATS: &[&str] = &["mdict", "stardict", "ecdict"];

/// 上传/目录导入按声明 format 做后缀白名单（mdict 额外放行附属资源扩展名）
fn allowed_extensions(format: &str) -> Vec<String> {
    match format {
        "mdict" => {
            let mut all = vec![".mdx".to_string(), ".mdd".to_string()];
            all.extend(SIBLING_RESOURCE_EXTENSIONS.iter().map(|s| s.to_string()));
            all
        }
        "stardict" => [".ifo", ".idx", ".dict", ".syn", ".dict.dz", ".idx.gz"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        "ecdict" => vec![".csv".to_string()],
        _ => Vec::new(),
    }
}

pub fn validate_format(format: &str) -> Result<(), AppError> {
    if VALID_FORMATS.contains(&format) {
        Ok(())
    } else {
        Err(AppError::validation(format!("不支持的词典格式：{format}")))
    }
}

pub fn validate_file_extensions(format: &str, paths: &[PathBuf]) -> Result<(), AppError> {
    let allowed = allowed_extensions(format);
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !allowed.iter().any(|ext| name.ends_with(ext.as_str())) {
            return Err(AppError::validation(format!(
                "文件 {} 的类型与所选格式「{format}」不匹配",
                path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            )));
        }
    }
    // ECDICT 一个 CSV 就是一部完整词典，选多个会静默只解析第一个
    if format == "ecdict" && paths.len() != 1 {
        return Err(AppError::validation(
            "ECDICT 格式只能选择一个 CSV 文件；多个 CSV 请分别单独导入",
        ));
    }
    Ok(())
}

// ── dict_entries 输出 ─────────────────────────────────────────────

pub fn dict_to_json(d: &crate::entities::dictionary::Model) -> Value {
    json!({
        "id": d.id,
        "name": d.name,
        "format": d.format,
        "lang_from": d.lang_from,
        "lang_to": d.lang_to,
        "word_count": d.word_count,
        "sort_order": d.sort_order,
        "status": d.status,
        "import_method": d.import_method,
        "imported_at": crate::core::timeutil::unix_to_iso(d.imported_at),
    })
}

pub async fn list_dictionaries(db: &DatabaseConnection) -> Result<Vec<crate::entities::dictionary::Model>, AppError> {
    use sea_orm::{EntityTrait, QueryOrder};
    Ok(crate::entities::dictionary::Entity::find()
        .order_by_asc(crate::entities::dictionary::Column::SortOrder)
        .order_by_asc(crate::entities::dictionary::Column::Id)
        .all(db)
        .await?)
}

async fn get_dictionary(
    db: &DatabaseConnection,
    dictionary_id: i32,
) -> Result<crate::entities::dictionary::Model, AppError> {
    use sea_orm::EntityTrait;
    crate::entities::dictionary::Entity::find_by_id(dictionary_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("词典不存在"))
}

// ── dicts_dir 扫描与分组 ─────────────────────────────────────────

/// 文件名后缀 → 词典格式（含双后缀 dict.dz / idx.gz）
fn format_by_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "mdx" | "mdd" => Some("mdict"),
        "ifo" | "idx" | "dict" | "syn" | "dict.dz" | "idx.gz" => Some("stardict"),
        "csv" => Some("ecdict"),
        _ => None,
    }
}

/// 一部词典的必需文件：外层「与」、内层「或」（.mdd/.syn 可选不计入）
fn required_extensions(format: &str) -> &'static [&'static [&'static str]] {
    match format {
        "mdict" => &[&["mdx"]],
        "stardict" => &[&["ifo"], &["idx", "idx.gz"], &["dict", "dict.dz"]],
        "ecdict" => &[&["csv"]],
        _ => &[],
    }
}

const DOUBLE_EXTENSIONS: &[&str] = &["dict.dz", "idx.gz"];
const ENTRY_EXTENSIONS: &[&str] = &["mdx", "ifo", "csv"];

/// 把文件名拆成 (格式, 分组键, 规范后缀)；不属于任何词典格式时 None。
/// 分组键带格式前缀且统一小写，foo.mdx 与 foo.ifo 不会并成一组。
fn split_dict_filename(filename: &str) -> Option<(String, String, String)> {
    let lowered = filename.to_lowercase();
    for ext in DOUBLE_EXTENSIONS {
        if lowered.ends_with(&format!(".{ext}")) {
            let stem = &filename[..filename.len() - ext.len() - 1];
            if stem.is_empty() {
                return None;
            }
            let format = format_by_ext(ext)?;
            return Some((
                format.to_string(),
                format!("{format}:{}", stem.to_lowercase()),
                (*ext).to_string(),
            ));
        }
    }
    let ext = lowered.rsplit('.').next().unwrap_or("").to_string();
    let format = format_by_ext(&ext)?;
    if filename.len() <= ext.len() + 1 {
        return None;
    }
    let stem = &filename[..filename.len() - ext.len() - 1];
    Some((
        format.to_string(),
        format!("{format}:{}", stem.to_lowercase()),
        ext,
    ))
}

fn missing_requirements(format: &str, exts: &[String]) -> Vec<String> {
    required_extensions(format)
        .iter()
        .filter(|alternatives| {
            !alternatives
                .iter()
                .any(|ext| exts.iter().any(|e| e == ext))
        })
        .map(|alternatives| {
            alternatives
                .iter()
                .map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>()
                .join(" 或 ")
        })
        .collect()
}

fn sanitize_dict_name(raw: &str, fallback: &str) -> String {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if name.is_empty() { fallback } else { &name };
    base.chars().take(255).collect()
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

/// dicts_dir 方式导入的已导入文件相对路径集合（dictionary_sources 表），
/// 用于比对「组内文件全部被消费过才算已导入」
async fn imported_dicts_dir_relpaths(
    db: &DatabaseConnection,
    inbox: &Path,
) -> Result<std::collections::HashSet<String>, AppError> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT s.path FROM dictionary_sources s \
             JOIN dictionaries d ON d.id = s.dictionary_id \
             WHERE d.import_method = 'dicts_dir'",
            [],
        ))
        .await?;
    let mut relpaths = std::collections::HashSet::new();
    for row in rows {
        let Ok(path) = row.try_get::<String>("", "path") else { continue };
        let Ok(resolved) = std::fs::canonicalize(&path) else { continue };
        let Ok(inbox_canon) = std::fs::canonicalize(inbox) else { continue };
        if let Ok(rel) = resolved.strip_prefix(&inbox_canon) {
            relpaths.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(relpaths)
}

/// 把前端传来的相对路径拼到 inbox 下并校验没有越权
pub fn resolve_dicts_subdir(inbox: &Path, subpath: &str) -> Result<PathBuf, AppError> {
    let parts: Vec<&str> = subpath.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return Err(AppError::validation(format!("非法路径：{subpath}")));
    }
    let target = if parts.is_empty() {
        inbox.to_path_buf()
    } else {
        inbox.join(parts.join("/"))
    };
    let target = target
        .canonicalize()
        .map_err(|_| AppError::validation(format!("非法路径：{subpath}")))?;
    let inbox_canon = inbox
        .canonicalize()
        .map_err(|_| AppError::validation(format!("非法路径：{subpath}")))?;
    if target != inbox_canon && !target.starts_with(&inbox_canon) {
        return Err(AppError::validation(format!("非法路径：{subpath}")));
    }
    Ok(target)
}

/// 白名单校验：只允许引用 inbox（含子目录）下已存在的文件，禁止路径穿越
pub fn resolve_dicts_dir_files(filenames: &[String], settings: &Settings) -> Result<Vec<PathBuf>, AppError> {
    let inbox_canon = std::fs::canonicalize(&settings.dicts_inbox_path)
        .map_err(|_| AppError::validation(format!("待导入目录不存在：{}", settings.dicts_inbox_path)))?;
    let mut resolved = Vec::new();
    for filename in filenames {
        if filename.is_empty() || filename.starts_with('/') || filename.contains('\\') {
            return Err(AppError::validation(format!("非法文件名：{filename}")));
        }
        let parts: Vec<&str> = filename.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() || parts.iter().any(|p| *p == "." || *p == "..") {
            return Err(AppError::validation(format!("非法文件名：{filename}")));
        }
        let candidate = inbox_canon.join(parts.join("/"));
        let Ok(canonical) = candidate.canonicalize() else {
            return Err(AppError::validation(format!("文件不存在于待导入目录：{filename}")));
        };
        if (canonical != inbox_canon && !canonical.starts_with(&inbox_canon))
            || !canonical.is_file()
        {
            return Err(AppError::validation(format!("文件不存在于待导入目录：{filename}")));
        }
        resolved.push(canonical);
    }
    Ok(resolved)
}

struct DictGroup {
    key: String,
    format: String,
    stem: String,
    paths: Vec<PathBuf>,
    exts: Vec<String>,
}

/// 把同一目录下的文件按 (格式, 主干) 归组。多卷 .mdd 仅在同目录存在对应 .mdx 时并组，
/// 孤立的 .mdd 单独成组并标缺件。
fn build_dict_groups(
    files: &[PathBuf],
    inbox: &Path,
    imported_relpaths: &std::collections::HashSet<String>,
    out_skipped: &mut Vec<String>,
) -> Vec<Value> {
    let mut mdx_stems: Vec<String> = Vec::new();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if let Some(stem) = name.strip_suffix(".mdx") {
            mdx_stems.push(stem.to_string());
        }
    }

    let mut grouped: Vec<DictGroup> = Vec::new();
    let mut group_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut sorted_files: Vec<&PathBuf> = files.iter().collect();
    sorted_files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());

    for path in sorted_files {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let Some((format, mut key, ext)) = split_dict_filename(&name) else {
            out_skipped.push(relpath_of(path, inbox));
            continue;
        };
        // MDict 资源分卷 X.1.mdd…：X.1 也可能是词典名本身，仅在能对上同目录 .mdx 主干时才剥卷号
        if format == "mdict" && ext == "mdd" {
            let stem = &name[..name.len() - ext.len() - 1];
            let volume_re = regex::Regex::new(r"\.\d+$").unwrap();
            let base = volume_re.replace(stem, "").into_owned();
            if base != stem && mdx_stems.contains(&base.to_lowercase()) {
                key = format!("mdict:{}", base.to_lowercase());
            }
        }
        let stem_name = name[..name.len() - ext.len() - 1].to_string();
        let index = *group_index.entry(key.clone()).or_insert_with(|| {
            grouped.push(DictGroup {
                key: key.clone(),
                format: format.clone(),
                stem: String::new(),
                paths: Vec::new(),
                exts: Vec::new(),
            });
            grouped.len() - 1
        });
        let group = &mut grouped[index];
        group.paths.push(path.clone());
        if !group.exts.contains(&ext) {
            group.exts.push(ext.clone());
        }
        // 主干取入口文件名，避免分卷的「X.1」被当成词典名
        if group.stem.is_empty() || ENTRY_EXTENSIONS.contains(&ext.as_str()) {
            group.stem = stem_name;
        }
    }

    let mut dictionaries = Vec::new();
    for group in &grouped {
        let mut group_files = group.paths.clone();
        group_files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
        let mut files_out = Vec::new();
        for path in &group_files {
            let relpath = relpath_of(path, inbox);
            files_out.push(json!({
                "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                "relpath": relpath,
                "size": file_size(path),
                "imported": imported_relpaths.contains(&relpath),
            }));
        }
        let missing = missing_requirements(&group.format, &group.exts);
        let all_imported = files_out.iter().all(|f| f["imported"].as_bool().unwrap_or(false));
        dictionaries.push(json!({
            "key": group.key,
            "name": suggest_dict_name(&group.format, &group.stem, &group_files),
            "format": group.format,
            "files": files_out,
            "total_size": group.paths.iter().map(|p| file_size(p)).sum::<i64>(),
            "importable": missing.is_empty(),
            "reason": if missing.is_empty() { Value::Null } else { json!(format!("缺少 {} 文件", missing.join("、"))) },
            "imported": all_imported,
        }));
    }
    dictionaries.sort_by_key(|d| d["name"].as_str().unwrap_or_default().to_lowercase());
    dictionaries
}

fn relpath_of(path: &Path, inbox: &Path) -> String {
    let Ok(inbox_canon) = inbox.canonicalize() else {
        return path.to_string_lossy().to_string();
    };
    let Ok(canonical) = path.canonicalize() else {
        return path.to_string_lossy().to_string();
    };
    match canonical.strip_prefix(&inbox_canon) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().to_string(),
    }
}

/// 建议名称：StarDict 的 .ifo 有 bookname 字段，其余用文件名主干
fn suggest_dict_name(format: &str, stem: &str, paths: &[PathBuf]) -> String {
    if format == "stardict" {
        if let Some(ifo) = paths
            .iter()
            .find(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase().ends_with(".ifo")).unwrap_or(false))
        {
            if let Ok(content) = std::fs::read_to_string(ifo) {
                let bookname = content
                    .lines()
                    .find_map(|line| {
                        let line = line.trim();
                        let (key, value) = line.split_once('=')?;
                        (key.trim() == "bookname").then(|| value.trim().to_string())
                    })
                    .unwrap_or_default();
                if !bookname.is_empty() {
                    return sanitize_dict_name(&bookname, stem);
                }
            }
        }
    }
    sanitize_dict_name(stem, stem)
}

/// 深度受限地遍历 root 及子目录；跳过 . 开头与符号链接目录
fn iter_scan_dirs(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        result.push(current.clone());
        if depth >= max_depth {
            continue;
        }
        let mut children: Vec<PathBuf> = match std::fs::read_dir(&current) {
            Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
            Err(err) => {
                tracing::warn!(dir = %current.display(), error = %err, "无法读取目录，已跳过");
                continue;
            }
        };
        children.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
        for child in children {
            if child.is_dir() && !child.is_symlink() {
                if let Some(name) = child.file_name() {
                    if !name.to_string_lossy().starts_with('.') {
                        stack.push((child, depth + 1));
                    }
                }
            }
        }
    }
    result
}

/// dicts_dir 列表（含分组）。返回 (normalized_path, entries, dictionaries, skipped)
pub async fn list_dicts_dir_files(
    state: &AppState,
    subpath: &str,
    recursive: bool,
) -> Result<(String, Vec<Value>, Vec<Value>, Vec<String>), AppError> {
    let inbox = std::fs::canonicalize(&state.cfg.dicts_inbox_path)
        .map_err(|e| AppError::validation(format!("待导入目录无效：{e}")))?;
    let target = resolve_dicts_subdir(&inbox, subpath)?;
    let normalized = if target == inbox {
        String::new()
    } else {
        target
            .strip_prefix(&inbox)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    };
    if !target.is_dir() {
        return Ok((normalized, Vec::new(), Vec::new(), Vec::new()));
    }

    let imported = imported_dicts_dir_relpaths(&state.db, &inbox).await?;

    // 文件系统遍历放阻塞线程（目录可能很大）
    let target2 = target.clone();
    let inbox2 = inbox.clone();
    let (dirs, files) = tokio::task::spawn_blocking(move || -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&target2) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.is_file() {
                    files.push(path);
                }
            }
        }
        let _ = &inbox2;
        (dirs, files)
    })
    .await
    .map_err(|e| AppError::internal("spawn", e))?;

    let mut entries = Vec::new();
    let mut sorted_dirs = dirs;
    sorted_dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    for path in &sorted_dirs {
        entries.push(json!({
            "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            "size": 0,
            "modified_at": mtime_iso(path),
            "imported": false,
            "is_dir": true,
        }));
    }
    let mut sorted_files = files;
    sorted_files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    for path in &sorted_files {
        let relpath = relpath_of(path, &inbox);
        entries.push(json!({
            "name": path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            "size": file_size(path),
            "modified_at": mtime_iso(path),
            "imported": imported.contains(&relpath),
            "is_dir": false,
        }));
    }

    let target3 = target.clone();
    let inbox3 = inbox.clone();
    let imported3 = imported.clone();
    let (dictionaries, skipped) = if recursive {
        tokio::task::spawn_blocking(move || {
            let mut dictionaries = Vec::new();
            let mut skipped = Vec::new();
            for current in iter_scan_dirs(&target3, MAX_SCAN_DEPTH) {
                let mut files: Vec<PathBuf> = match std::fs::read_dir(&current) {
                    Ok(entries) => entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect(),
                    Err(_) => continue,
                };
                if files.is_empty() {
                    continue;
                }
                files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
                let mut groups = build_dict_groups(&files, &inbox3, &imported3, &mut skipped);
                // 扫描起点之外、目录里恰好只有一部可导入词典时，用目录名当名称
                let importable_count = groups
                    .iter()
                    .filter(|g| g["importable"].as_bool().unwrap_or(false))
                    .count();
                if current != target3 && importable_count == 1 {
                    if let Some(group) = groups
                        .iter_mut()
                        .find(|g| g["importable"].as_bool().unwrap_or(false))
                    {
                        let dir_name = current
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        group["name"] = json!(sanitize_dict_name(
                            &dir_name,
                            group["name"].as_str().unwrap_or_default(),
                        ));
                    }
                }
                let dir_label = if current == inbox3 {
                    String::new()
                } else {
                    current
                        .strip_prefix(&inbox3)
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default()
                };
                for group in groups.iter_mut() {
                    group["dir"] = json!(dir_label);
                }
                dictionaries.extend(groups);
            }
            skipped.sort();
            (dictionaries, skipped)
        })
        .await
        .map_err(|e| AppError::internal("spawn", e))?
    } else {
        let mut skipped = Vec::new();
        let mut groups = build_dict_groups(&sorted_files, &inbox, &imported, &mut skipped);
        for group in groups.iter_mut() {
            group["dir"] = json!(normalized);
        }
        (groups, skipped)
    };
    let mut dictionaries = dictionaries;
    if recursive {
        dictionaries.sort_by_key(|d| {
            (
                d["dir"].as_str().unwrap_or_default().to_string(),
                d["name"].as_str().unwrap_or_default().to_lowercase(),
            )
        });
    }
    Ok((normalized, entries, dictionaries, skipped))
}

fn mtime_iso(path: &Path) -> String {
    use std::time::UNIX_EPOCH;
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| crate::core::timeutil::unix_to_iso(d.as_secs() as i64))
        .unwrap_or_else(|| crate::core::timeutil::unix_to_iso(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_filename_groups_by_format_and_stem() {
        let (format, key, ext) = split_dict_filename("Foo.MDX").unwrap();
        assert_eq!(format, "mdict");
        assert_eq!(key, "mdict:foo");
        assert_eq!(ext, "mdx");

        let (format, key, ext) = split_dict_filename("langdao.dict.dz").unwrap();
        assert_eq!(format, "stardict");
        assert_eq!(key, "stardict:langdao");
        assert_eq!(ext, "dict.dz");

        let (format, _, _) = split_dict_filename("x.idx.gz").unwrap();
        assert_eq!(format, "stardict");

        assert!(split_dict_filename("readme.txt").is_none());
        assert!(split_dict_filename("csv").is_none());
    }

    #[test]
    fn missing_requirements_lists_alternatives() {
        let missing = missing_requirements("stardict", &["ifo".to_string(), "idx".to_string()]);
        assert_eq!(missing, vec![".dict 或 .dict.dz"]);
        assert!(missing_requirements("mdict", &["mdx".to_string()]).is_empty());
    }

    #[test]
    fn sanitize_collapses_whitespace_and_caps() {
        assert_eq!(sanitize_dict_name("  a   b  ", "fb"), "a b");
        assert_eq!(sanitize_dict_name("", "fb"), "fb");
        let long = "x".repeat(300);
        assert_eq!(sanitize_dict_name(&long, "fb").chars().count(), 255);
    }
}

// ── 导入管线 ─────────────────────────────────────────────────────

pub struct ImportParams {
    pub name: String,
    pub format: String,
    pub lang_from: Option<String>,
    pub lang_to: Option<String>,
    pub staged_paths: Vec<PathBuf>,
    pub admin_id: i32,
    pub import_method: String, // "upload" | "dicts_dir"
    /// true = 连释义引用都不改写（保留 sound:// 等原文）
    pub skip_resources: bool,
    /// true = 额外把 .mdd 全量解包到 res/（磁盘换时延的可选项；默认 false，
    /// 运行期由 /dict-res 直接读 .mdd）
    pub extract_resources: bool,
}

/// 校验参数后把解析入库丢进后台任务，立即返回任务 id 供前端轮询。
/// 格式/文件名校验很快，留在请求线程里同步做，坏输入当次请求就报错。
pub fn start_dictionary_import(
    state: &Arc<AppState>,
    params: ImportParams,
) -> Result<i64, AppError> {
    validate_format(&params.format)?;
    validate_file_extensions(&params.format, &params.staged_paths)?;
    let task_id = state.tasks.start("dictionary_import", &params.name, false);
    let state = state.clone();
    tokio::spawn(async move {
        // SQLite 单写者：导入/重解析/修复/VACUUM 排队执行而非互相报锁
        let _guard = state.bulk_write.lock().await;
        match run_import(&state, task_id, params).await {
            Ok(result) => state.tasks.succeed(task_id, result),
            Err(err) => state.tasks.fail(task_id, err.message),
        }
    });
    Ok(task_id)
}

async fn run_import(state: &Arc<AppState>, task_id: i64, params: ImportParams) -> Result<Value, AppError> {
    let dictionary = import_dictionary(state, task_id, &params).await?;
    Ok(json!({
        "dictionary_id": dictionary.id,
        "word_count": dictionary.word_count,
        "lang_from": dictionary.lang_from,
        "lang_to": dictionary.lang_to,
    }))
}

/// 采样识别语言（None 的侧）；采样失败回落默认方向，不连累导入
async fn resolve_languages(
    _state: &AppState,
    params: &ImportParams,
) -> Result<(String, String), AppError> {
    if params.lang_from.is_some() && params.lang_to.is_some() {
        return Ok((
            params.lang_from.clone().unwrap(),
            params.lang_to.clone().unwrap(),
        ));
    }
    let format = params.format.clone();
    let paths = params.staged_paths.clone();
    let detected = tokio::task::spawn_blocking(move || -> std::result::Result<_, dict_parser::ParserError> {
        let mut parser = parser_by_format(&format)?;
        let headwords = parser.sample_headwords(&paths, SAMPLE_HEADWORD_LIMIT)?;
        let sampled = parser.sample(&paths, SAMPLE_DEFINITION_LIMIT)?;
        Ok(detect_language(
            &headwords,
            &sampled.iter().map(|e| e.definition.clone()).collect::<Vec<_>>(),
        ))
    })
    .await
    .map_err(|e| AppError::internal("spawn", e))?;

    let (detected_from, detected_to) = match detected {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(error = %err, "语言方向自动识别失败，回落到默认值");
            (None, None)
        }
    };
    Ok((
        params
            .lang_from
            .clone()
            .or_else(|| detected_from.map(String::from))
            .unwrap_or_else(|| FALLBACK_LANG_FROM.to_string()),
        params
            .lang_to
            .clone()
            .or_else(|| detected_to.map(String::from))
            .unwrap_or_else(|| FALLBACK_LANG_TO.to_string()),
    ))
}

async fn import_dictionary(
    state: &Arc<AppState>,
    task_id: i64,
    params: &ImportParams,
) -> Result<crate::entities::dictionary::Model, AppError> {
    let (lang_from, lang_to) = resolve_languages(state, params).await?;
    let now = chrono::Utc::now().timestamp();

    // 先插 dictionaries 行拿自增 id（资源目录与 HTML 改写要用）
    use sea_orm::{Set, EntityTrait};
    let record = crate::entities::dictionary::ActiveModel {
        name: Set(params.name.clone()),
        format: Set(params.format.clone()),
        lang_from: Set(lang_from.clone()),
        lang_to: Set(lang_to.clone()),
        import_method: Set(params.import_method.clone()),
        status: Set("disabled".to_string()),
        imported_by: Set(Some(params.admin_id)),
        imported_at: Set(now),
        ..Default::default()
    };
    let dictionary = crate::entities::dictionary::Entity::insert(record)
        .exec_with_returning(&state.db)
        .await?;
    let dict_id = dictionary.id;

    let storage_root = Path::new(&state.cfg.dictionary_storage_path).join(dict_id.to_string());
    // 磁盘优化：默认不解包 .mdd（/dict-res 直接读）；引用改写与解包独立控制
    let resource_dir = if params.extract_resources && !params.skip_resources {
        Some(storage_root.join("res"))
    } else {
        None
    };

    let result = import_entries_into(
        state,
        task_id,
        dict_id,
        &params.format,
        &params.staged_paths,
        resource_dir,
        !params.skip_resources, // 是否改写释义里的资源引用
        true,                   // overwrite resources
        0,                      // generation 0
        false,                  // 单事务（导入失败整体撤销）
    )
    .await;

    let word_count = match result {
        Ok(count) => count,
        Err(err) => {
            // 词条尚未提交任何代 → 删词典行即可；再清理已落盘的资源目录
            let _ = crate::entities::dictionary::Entity::delete_by_id(dict_id)
                .exec(&state.db)
                .await;
            let _ = std::fs::remove_dir_all(&storage_root);
            return Err(err);
        }
    };

    // 成功：登记源文件清单（upload 移动归档进 source/；dicts_dir 记录原路径）
    let final_paths: Vec<PathBuf> = if params.import_method == "upload" {
        let source_dir = storage_root.join("source");
        tokio::task::spawn_blocking({
            let source_dir = source_dir.clone();
            let staged = params.staged_paths.clone();
            move || -> std::io::Result<Vec<PathBuf>> {
                std::fs::create_dir_all(&source_dir)?;
                let mut moved = Vec::new();
                for path in &staged {
                    let target = source_dir.join(
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    );
                    std::fs::rename(path, &target).or_else(|_| {
                        // 跨设备时 fallback 到 copy+delete
                        std::fs::copy(path, &target)?;
                        std::fs::remove_file(path)
                    })?;
                    moved.push(target);
                }
                Ok(moved)
            }
        })
        .await
        .map_err(|e| AppError::internal("spawn", e))?
        .map_err(|e: std::io::Error| AppError::internal("move-files", e))?;
        moved_paths_to_source_dir(&source_dir)
    } else {
        params.staged_paths.clone()
    };
    record_source_files(&state.db, dict_id, &final_paths).await?;

    // 更新 word_count
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE dictionaries SET word_count = $1 WHERE id = $2",
            [word_count.into(), dict_id.into()],
        ))
        .await?;

    audit_service::log_action(
        &state.db,
        "admin",
        Some(params.admin_id),
        "dictionary.import",
        Some(&dict_id.to_string()),
        Some(json!({
            "name": params.name,
            "format": params.format,
            "word_count": word_count,
            "lang_from": lang_from,
            "lang_to": lang_to,
            "skip_resources": params.skip_resources,
        })),
    )
    .await?;

    state.query_cache.invalidate();
    get_dictionary(&state.db, dict_id).await
}

fn moved_paths_to_source_dir(source_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(source_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// dictionary_sources 子表登记源文件清单（保序）
async fn record_source_files(
    db: &DatabaseConnection,
    dictionary_id: i32,
    paths: &[PathBuf],
) -> Result<(), AppError> {
    for (position, path) in paths.iter().enumerate() {
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO dictionary_sources (dictionary_id, position, path) VALUES ($1, $2, $3)",
            [
                dictionary_id.into(),
                (position as i32).into(),
                path.to_string_lossy().to_string().into(),
            ],
        ))
        .await?;
    }
    Ok(())
}

/// 解析 + 写库的核心：生产者（阻塞线程解析）→ channel → 消费者（批量 INSERT）。
/// generation 写指定代；commit_each_batch=true 时每批一个短事务（重解析写下一代用），
/// 否则整部词典一个事务（导入用，失败整体撤销）。
#[allow(clippy::too_many_arguments)]
async fn import_entries_into(
    state: &Arc<AppState>,
    task_id: i64,
    dict_id: i32,
    format: &str,
    staged_paths: &[PathBuf],
    resource_dir: Option<PathBuf>,
    rewrite_refs: bool,
    overwrite_resources: bool,
    generation: i32,
    commit_each_batch: bool,
) -> Result<i64, AppError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<dict_parser::ParsedEntry>>(4);

    let format = format.to_string();
    let files = staged_paths.to_vec();
    let mut opts = ParseOpts::new(dict_id);
    if rewrite_refs {
        opts = opts.with_rewrite();
    }
    if let Some(dir) = &resource_dir {
        opts = opts.with_resources(dir.clone(), overwrite_resources);
    }
    let producer = tokio::task::spawn_blocking(move || -> dict_parser::Result<()> {
        let mut parser = parser_by_format(&format)?;
        parser.parse(&files, &opts, &mut |batch| {
            tx.blocking_send(batch)
                .map_err(|_| dict_parser::ParserError::Internal("导入消费端已退出".into()))
        })
    });

    let mut count: i64 = 0;
    let mut error: Option<AppError> = None;
    // 导入 = 单事务；重解析 = 每批提交
    let mut tx = if commit_each_batch {
        None
    } else {
        Some(state.db.begin().await.map_err(AppError::from)?)
    };
    while let Some(batch) = rx.recv().await {
        enum BatchOutcome {
            Ok,
            Err(AppError),
        }
        let outcome = match &tx {
            Some(t) => match insert_batch(t, dict_id, &batch, generation).await {
                Ok(()) => BatchOutcome::Ok,
                Err(e) => BatchOutcome::Err(e),
            },
            None => match insert_batch(&state.db, dict_id, &batch, generation).await {
                Ok(()) => BatchOutcome::Ok,
                Err(e) => BatchOutcome::Err(e),
            },
        };
        match outcome {
            BatchOutcome::Ok => {
                count += batch.len() as i64;
                if commit_each_batch && count % (BATCH_SIZE as i64 * 8) == 0 {
                    let mut progress = Map::new();
                    progress.insert("done".into(), json!(count));
                    state.tasks.update_progress(task_id, progress);
                }
            }
            BatchOutcome::Err(err) => {
                error = Some(err);
                break;
            }
        }
    }
    if let Some(t) = tx.take() {
        if error.is_none() {
            t.commit().await.map_err(AppError::from)?;
        } else {
            let _ = t.rollback().await;
        }
    }
    // 等待生产者结束，拿到解析侧错误（若有）
    match producer.await {
        Ok(Ok(())) => {}
        Ok(Err(parse_err)) => {
            let err = match parse_err {
                dict_parser::ParserError::Validation(msg) => AppError::validation(msg),
                dict_parser::ParserError::Unsupported(msg) => AppError::validation(msg),
                other => AppError::internal("parse", other),
            };
            return Err(error.unwrap_or(err));
        }
        Err(join_err) => return Err(error.unwrap_or_else(|| AppError::internal("join", join_err))),
    }
    if let Some(err) = error {
        return Err(err);
    }
    let mut progress = Map::new();
    progress.insert("done".into(), json!(count));
    state.tasks.update_progress(task_id, progress);
    Ok(count)
}

/// 一批词条（≤2000 行）按 500 行/语句切分做多值 INSERT。
/// SQLite 参数上限 32766，500×7=3500 安全。
async fn insert_batch<C: ConnectionTrait>(
    conn: &C,
    dictionary_id: i32,
    batch: &[dict_parser::ParsedEntry],
    generation: i32,
) -> Result<(), AppError> {
    const ROWS_PER_STMT: usize = 500;
    let backend = conn.get_database_backend();
    for chunk in batch.chunks(ROWS_PER_STMT) {
        let mut sql = String::with_capacity(128 + chunk.len() * 80);
        sql.push_str(
            "INSERT INTO dict_entries (dictionary_id, word, word_lower, phonetic, definition, extra, generation) VALUES ",
        );
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(chunk.len() * 7);
        let mut param_index = 1usize;
        for (i, entry) in chunk.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('(');
            for col in 0..7 {
                if col > 0 {
                    sql.push(',');
                }
                match backend {
                    sea_orm::DbBackend::Postgres => sql.push_str(&format!("${param_index}")),
                    _ => sql.push('?'),
                }
                param_index += 1;
            }
            sql.push(')');
            values.push(dictionary_id.into());
            values.push(entry.word.clone().into());
            values.push(entry.word.to_lowercase().into());
            values.push(entry.phonetic.clone().into());
            values.push(entry.definition.clone().into());
            values.push(entry.extra.as_ref().map(|v| v.to_string()).into());
            values.push(generation.into());
        }
        conn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
            .await?;
    }
    Ok(())
}

// ── 重新解析（代切换）─────────────────────────────────────────────

/// 取回重新解析所需的源文件路径：
/// dicts_dir 存源文件绝对路径清单；upload 存 source/ 归档目录（取目录内全部文件）
pub async fn source_paths_for(db: &DatabaseConnection, dictionary_id: i32) -> Result<Vec<PathBuf>, AppError> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT s.path, d.import_method FROM dictionary_sources s \
             JOIN dictionaries d ON d.id = s.dictionary_id WHERE s.dictionary_id = $1 ORDER BY s.position",
            [dictionary_id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let import_method = rows[0]
        .try_get::<String>("", "import_method")
        .unwrap_or_default();
    if import_method == "upload" {
        // path 都是 source/ 目录下的归档文件
        return Ok(rows
            .iter()
            .filter_map(|r| r.try_get::<String>("", "path").ok())
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .collect());
    }
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "path").ok())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .collect())
}

/// 管理端 API 用：返回目标词典 id 列表（None = 全部；任一不存在整体 404）
pub async fn resolve_target_ids_for_admin(
    db: &DatabaseConnection,
    dictionary_ids: Option<&[i32]>,
) -> Result<Vec<i32>, AppError> {
    Ok(resolve_target_dictionaries(db, dictionary_ids)
        .await?
        .iter()
        .map(|d| d.id)
        .collect())
}

/// 解析要处理的词典；传了 id 就按传入顺序返回，任一个不存在整体拒绝
async fn resolve_target_dictionaries(
    db: &DatabaseConnection,
    dictionary_ids: Option<&[i32]>,
) -> Result<Vec<crate::entities::dictionary::Model>, AppError> {
    match dictionary_ids {
        None => list_dictionaries(db).await,
        Some(ids) => {
            let mut unique: Vec<i32> = Vec::new();
            for id in ids {
                if !unique.contains(id) {
                    unique.push(*id);
                }
            }
            let mut found = std::collections::HashMap::new();
            for dict in list_dictionaries(db).await? {
                if unique.contains(&dict.id) {
                    found.insert(dict.id, dict);
                }
            }
            let missing: Vec<i32> = unique.iter().copied().filter(|id| !found.contains_key(id)).collect();
            if !missing.is_empty() {
                return Err(AppError::not_found(format!("包含不存在的词典 ID：{missing:?}")));
            }
            Ok(unique.into_iter().map(|id| found.remove(&id).expect("checked")).collect())
        }
    }
}

/// 登记「重新解析」后台任务；dictionary_ids 为空表示全部词典。
/// 修「同名词词条被 UNIQUE 去重丢掉」的历史数据用：全量重灌新一代、原子切换。
pub async fn start_reparse(
    state: &Arc<AppState>,
    dictionary_ids: Option<&[i32]>,
) -> Result<i64, AppError> {
    let targets = resolve_target_dictionaries(&state.db, dictionary_ids).await?;
    let target_ids: Vec<i32> = targets.iter().map(|d| d.id).collect();
    {
        let mut reparsing = state.reparsing.lock().unwrap_or_else(|e| e.into_inner());
        let busy: Vec<i32> = target_ids.iter().copied().filter(|id| reparsing.contains(id)).collect();
        if !busy.is_empty() {
            return Err(AppError::conflict(format!("词典 {busy:?} 正在重新解析，请等它结束")));
        }
        for id in &target_ids {
            reparsing.insert(*id);
        }
    }
    let task_id = state.tasks.start("dictionary_reparse", "重新解析词典", false);
    let state2 = state.clone();
    tokio::spawn(async move {
        let _guard = state2.bulk_write.lock().await;
        let result = run_reparse(&state2, task_id, target_ids.clone()).await;
        let mut reparsing = state2.reparsing.lock().unwrap_or_else(|e| e.into_inner());
        for id in &target_ids {
            reparsing.remove(id);
        }
        match result {
            Ok(value) => state2.tasks.succeed(task_id, value),
            Err(err) => state2.tasks.fail(task_id, err.message),
        }
    });
    Ok(task_id)
}

async fn run_reparse(state: &Arc<AppState>, task_id: i64, dictionary_ids: Vec<i32>) -> Result<Value, AppError> {
    let total = dictionary_ids.len();
    let mut reparsed = 0i64;
    let mut skipped = 0i64;
    let mut entries_total = 0i64;
    for (idx, dict_id) in dictionary_ids.iter().enumerate() {
        let index = (idx + 1) as i64;
        let Ok(Some(dictionary)) = (|| async {
            use sea_orm::EntityTrait;
            crate::entities::dictionary::Entity::find_by_id(*dict_id)
                .one(&state.db)
                .await
                .map_err(AppError::from)
        })()
        .await
        else {
            skipped += 1;
            let mut progress = Map::new();
            progress.insert("done".into(), json!(index));
            progress.insert("total".into(), json!(total));
            state.tasks.update_progress(task_id, progress);
            continue;
        };
        let paths = source_paths_for(&state.db, dictionary.id).await?;
        if paths.is_empty() {
            // 源文件不在（导入后挪走了/删了）——跳过并计数
            skipped += 1;
            let mut progress = Map::new();
            progress.insert("done".into(), json!(index));
            progress.insert("total".into(), json!(total));
            state.tasks.update_progress(task_id, progress);
            continue;
        }
        let count = reparse_one(state, task_id, &dictionary, &paths, index, total).await?;
        reparsed += 1;
        entries_total += count;
        let mut progress = Map::new();
        progress.insert("done".into(), json!(index));
        progress.insert("total".into(), json!(total));
        state.tasks.update_progress(task_id, progress);
    }
    Ok(json!({
        "dictionaries": reparsed,
        "entries": entries_total,
        "skipped": skipped,
    }))
}

/// 重新解析一部词典，返回新的词条数。
/// 1. 写下一代（每批提交，查询不可见）；2. 改 active_generation 一行原子切换；
/// 3. 分批删旧代。资源按「只补缺失、不覆盖」处理。
async fn reparse_one(
    state: &Arc<AppState>,
    task_id: i64,
    dictionary: &crate::entities::dictionary::Model,
    paths: &[PathBuf],
    _index: i64,
    _total: usize,
) -> Result<i64, AppError> {
    let dict_id = dictionary.id;
    let current = dictionary.active_generation;
    // 上次中断留下的半代
    purge_generations(&state.db, dict_id, Some(current), false).await?;

    let next_generation = current + 1;
    let res_dir = Path::new(&state.cfg.dictionary_storage_path)
        .join(dict_id.to_string())
        .join("res");
    // 勾了「不导入发音/图片」的词典没有 res/，resource_dir=None：释义不做引用改写，
    // 与当初导入时的行为一致
    let resource_dir = res_dir.is_dir().then_some(res_dir);

    let result = import_entries_into(
        state,
        task_id,
        dict_id,
        &dictionary.format,
        paths,
        resource_dir,
        true,  // 引用始终改写（与默认导入一致）
        false, // 资源只补缺失
        next_generation,
        true,  // 每批提交
    )
    .await;
    let count = match result {
        Ok(count) => count,
        Err(err) => {
            // 失败只清刚写的下一代，旧代原封不动、可重跑
            purge_generations(&state.db, dict_id, Some(next_generation), true).await?;
            return Err(err);
        }
    };

    // 原子切换：只改一行
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE dictionaries SET active_generation = $1, word_count = $2 WHERE id = $3",
            [next_generation.into(), count.into(), dict_id.into()],
        ))
        .await?;
    // 缓存里还是上一代的条目（条目 id 已换）
    state.query_cache.invalidate();
    purge_generations(&state.db, dict_id, Some(next_generation), false).await?;
    // 旧一代清完后主键区间才稳定，这时才能失效随机区间缓存
    state.random_bounds.invalidate_all();
    Ok(count)
}

/// 按主键区间分批删词典词条。keep_generation=Some(g) + exclude=true → 只删 g 这一代
/// （失败清理用）；exclude=false → 删除除 g 以外的所有代（清旧代/半代用）。
/// None → 删除该词典全部。
async fn purge_generations(
    db: &DatabaseConnection,
    dictionary_id: i32,
    keep_generation: Option<i32>,
    exclude: bool,
) -> Result<(), AppError> {
    let backend = db.get_database_backend();
    let (condition_sql, condition_params): (String, Vec<sea_orm::Value>) = match keep_generation {
        Some(g) if exclude => (" AND generation = $2".to_string(), vec![g.into()]),
        Some(g) => (" AND generation != $2".to_string(), vec![g.into()]),
        None => (String::new(), vec![]),
    };
    // 主键区间（`dictionary_id + 0 = ?` 强制走主键，禁用 dictionary_id 索引）
    let bounds_sql = format!(
        "SELECT MIN(id), MAX(id) FROM dict_entries WHERE dictionary_id + 0 = $1{condition_sql}"
    );
    let mut bound_params: Vec<sea_orm::Value> = vec![dictionary_id.into()];
    bound_params.extend(condition_params.clone());
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            bounds_sql,
            bound_params,
        ))
        .await?;
    let Some(row) = row else { return Ok(()) };
    let lowest: Option<i64> = row.try_get("", "MIN(id)").ok().flatten();
    let highest: Option<i64> = row.try_get("", "MAX(id)").ok().flatten();
    let (Some(lowest), Some(highest)) = (lowest, highest) else {
        return Ok(());
    };
    let mut cursor = lowest - 1;
    while cursor < highest {
        let upper = std::cmp::min(cursor + PURGE_BATCH_SIZE, highest);
        let gen_condition = if condition_sql.is_empty() {
            String::new()
        } else {
            condition_sql.replace("$2", "$3")
        };
        let sql = format!(
            "DELETE FROM dict_entries WHERE dictionary_id + 0 = $1 AND id > $2 AND id <= $3{gen_condition}"
        );
        let mut params: Vec<sea_orm::Value> = vec![dictionary_id.into(), cursor.into(), upper.into()];
        params.extend(condition_params.clone());
        db.execute_raw(Statement::from_sql_and_values(backend, sql, params))
            .await?;
        cursor = upper;
    }
    Ok(())
}

// ── CRUD ─────────────────────────────────────────────────────────

pub async fn set_dictionary_status(
    state: &AppState,
    dictionary_id: i32,
    status: &str,
    admin_id: i32,
) -> Result<Value, AppError> {
    get_dictionary(&state.db, dictionary_id).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE dictionaries SET status = $1 WHERE id = $2",
            [status.into(), dictionary_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        &format!("dictionary.{status}"),
        Some(&dictionary_id.to_string()),
        None,
    )
    .await?;
    state.query_cache.invalidate();
    let dict = get_dictionary(&state.db, dictionary_id).await?;
    Ok(dict_to_json(&dict))
}

pub async fn set_dictionaries_status(
    state: &AppState,
    dictionary_ids: &[i32],
    status: &str,
    admin_id: i32,
) -> Result<Vec<Value>, AppError> {
    // 重复 id 去重；任一不存在整批拒绝
    let mut unique: Vec<i32> = Vec::new();
    for id in dictionary_ids {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }
    for id in &unique {
        get_dictionary(&state.db, *id).await?;
    }
    for id in &unique {
        state
            .db
            .execute_raw(Statement::from_sql_and_values(
                state.db.get_database_backend(),
                "UPDATE dictionaries SET status = $1 WHERE id = $2",
                [status.into(), (*id).into()],
            ))
            .await?;
        audit_service::log_action(
            &state.db,
            "admin",
            Some(admin_id),
            &format!("dictionary.{status}"),
            Some(&id.to_string()),
            None,
        )
        .await?;
    }
    state.query_cache.invalidate();
    Ok(list_dictionaries(&state.db)
        .await?
        .iter()
        .map(dict_to_json)
        .collect())
}

pub async fn update_dictionary_metadata(
    state: &AppState,
    dictionary_id: i32,
    name: &str,
    lang_from: &str,
    lang_to: &str,
    admin_id: i32,
) -> Result<Value, AppError> {
    get_dictionary(&state.db, dictionary_id).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "UPDATE dictionaries SET name = $1, lang_from = $2, lang_to = $3 WHERE id = $4",
            [
                name.into(),
                lang_from.into(),
                lang_to.into(),
                dictionary_id.into(),
            ],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "dictionary.update",
        Some(&dictionary_id.to_string()),
        Some(json!({"name": name, "lang_from": lang_from, "lang_to": lang_to})),
    )
    .await?;
    // lang_from 决定查询路由、缓存里带词典名快照 → 全量失效
    state.query_cache.invalidate();
    let dict = get_dictionary(&state.db, dictionary_id).await?;
    Ok(dict_to_json(&dict))
}

/// 按正则批量重命名（fancy-regex 支持用户输入的 Python re 语法；\N 反向引用转换成 $N）
pub async fn rename_dictionaries(
    state: &AppState,
    pattern: &str,
    replacement: &str,
    dictionary_ids: Option<&[i32]>,
    dry_run: bool,
    admin_id: i32,
) -> Result<Value, AppError> {
    let compiled = fancy_regex::Regex::new(pattern)
        .map_err(|e| AppError::validation(format!("正则表达式无效：{e}")))?;
    // Python 替换串 \1 → regex crate 的 ${1}
    let rust_replacement = convert_python_replacement(replacement);

    let dicts = match dictionary_ids {
        None => list_dictionaries(&state.db).await?,
        Some(ids) => resolve_target_dictionaries(&state.db, Some(ids)).await?,
    };
    let mut items = Vec::new();
    for dict in &dicts {
        let new_name = compiled
            .replace_all(&dict.name, rust_replacement.as_str())
            .trim()
            .to_string();
        // 改成空名/超长名/未变的跳过
        if new_name.is_empty() || new_name == dict.name || new_name.chars().count() > 255 {
            continue;
        }
        items.push(json!({"id": dict.id, "name": dict.name, "new_name": new_name}));
    }

    if !dry_run && !items.is_empty() {
        for item in &items {
            state
                .db
                .execute_raw(Statement::from_sql_and_values(
                    state.db.get_database_backend(),
                    "UPDATE dictionaries SET name = $1 WHERE id = $2",
                    [item["new_name"].as_str().unwrap_or_default().into(), item["id"].as_i64().unwrap_or_default().into()],
                ))
                .await?;
        }
        for item in &items {
            audit_service::log_action(
                &state.db,
                "admin",
                Some(admin_id),
                "dictionary.rename",
                Some(&item["id"].to_string()),
                Some(json!({"from": item["name"], "to": item["new_name"]})),
            )
            .await?;
        }
        state.query_cache.invalidate();
    }
    Ok(json!({"items": items, "applied": !dry_run}))
}

/// Python re.sub 替换串的 \1 / \g<1> → fancy-regex 的 ${1}
fn convert_python_replacement(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next.is_ascii_digit() {
                out.push_str(&format!("${{{next}}}"));
                i += 2;
                continue;
            }
            if next == 'g' && chars.get(i + 2) == Some(&'<') {
                if let Some(close) = chars[i + 3..].iter().position(|&c| c == '>') {
                    let name: String = chars[i + 3..i + 3 + close].iter().collect();
                    out.push_str(&format!("${{{name}}}"));
                    i += 4 + close;
                    continue;
                }
            }
            // 转义字符（\\、\$ 等）原样保留
            out.push(c);
            out.push(next);
            i += 2;
            continue;
        }
        // regex crate 用 $ 做展开，用户字面 $ 需转义为 $$
        if c == '$' {
            out.push_str("$$");
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

pub async fn reorder_dictionaries(
    state: &AppState,
    ordered_ids: &[i32],
    admin_id: i32,
) -> Result<Vec<Value>, AppError> {
    if ordered_ids.is_empty() {
        return Err(AppError::validation("排序列表不能为空"));
    }
    let dicts = list_dictionaries(&state.db).await?;
    let known: std::collections::HashMap<i32, ()> =
        dicts.iter().map(|d| (d.id, ())).collect();
    if !ordered_ids.iter().all(|id| known.contains_key(id)) {
        return Err(AppError::conflict("排序列表包含不存在的词典 ID"));
    }
    for (index, dict_id) in ordered_ids.iter().enumerate() {
        state
            .db
            .execute_raw(Statement::from_sql_and_values(
                state.db.get_database_backend(),
                "UPDATE dictionaries SET sort_order = $1 WHERE id = $2",
                [(index as i32).into(), (*dict_id).into()],
            ))
            .await?;
    }
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "dictionary.reorder",
        None,
        Some(json!({"order": ordered_ids})),
    )
    .await?;
    Ok(list_dictionaries(&state.db).await?.iter().map(dict_to_json).collect())
}

pub async fn delete_dictionary(
    state: &AppState,
    dictionary_id: i32,
    admin_id: i32,
) -> Result<(), AppError> {
    let dictionary = get_dictionary(&state.db, dictionary_id).await?;
    let import_method = dictionary.import_method.clone();

    // 生词本/查询日志只把词典当来源参考（生词本自带释义快照），先解除引用；
    // grants 表有 CASCADE；dict_entries 也 CASCADE
    let backend = state.db.get_database_backend();
    for table in ["vocab_items", "token_vocab_items", "query_logs"] {
        state
            .db
            .execute_raw(Statement::from_string(
                backend,
                format!("UPDATE {table} SET dictionary_id = NULL WHERE dictionary_id = {dictionary_id}"),
            ))
            .await?;
    }
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM dictionary_sources WHERE dictionary_id = $1",
            [dictionary_id.into()],
        ))
        .await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM dictionaries WHERE id = $1",
            [dictionary_id.into()],
        ))
        .await?;
    audit_service::log_action(
        &state.db,
        "admin",
        Some(admin_id),
        "dictionary.delete",
        Some(&dictionary_id.to_string()),
        None,
    )
    .await?;

    // res/ 是解析时提取的派生资源，与导入方式无关，直接清理；
    // source/ 只有 upload 方式才归本应用管；dicts_dir 的源文件不碰
    let storage_root = Path::new(&state.cfg.dictionary_storage_path).join(dictionary_id.to_string());
    {
        let storage_root2 = storage_root.clone();
        let import_method2 = import_method.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let resource_dir = storage_root2.join("res");
            if resource_dir.exists() {
                let _ = std::fs::remove_dir_all(resource_dir);
            }
            if import_method2 == "upload" {
                let source_dir = storage_root2.join("source");
                if source_dir.exists() {
                    let _ = std::fs::remove_dir_all(source_dir);
                }
            }
            if storage_root2.exists() {
                if let Ok(mut entries) = std::fs::read_dir(&storage_root2) {
                    if entries.next().is_none() {
                        let _ = std::fs::remove_dir(&storage_root2);
                    }
                }
            }
        })
        .await;
    }

    state.query_cache.invalidate();
    state.random_bounds.invalidate_all();
    state.mdd_resources.invalidate_dictionary(dictionary_id);
    schedule_vacuum(state);
    Ok(())
}

/// VACUUM 排队去重：已在排队时不必再加，开跑时会一并回收空闲页
fn schedule_vacuum(state: &AppState) {
    if state
        .vacuum_pending
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let cfg = state.cfg.clone();
    let database_url = cfg.database_url();
    let is_sqlite = cfg.is_sqlite();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let run = async {
            // 回收删除词条后 SQLite 的空闲页。VACUUM 不能在事务内跑，用独立裸连接；
            // WAL 模式下还要 TRUNCATE checkpoint 文件长度才会真正降下来
            let conn = match crate::core::db::raw_connection_url(&database_url, is_sqlite).await {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(error = %err, "VACUUM 连接失败");
                    return;
                }
            };
            if let Err(err) = conn.execute_unprepared("VACUUM").await {
                tracing::error!(error = %err, "VACUUM 失败，磁盘空间下次删除词典时会重试回收");
            }
            let _ = conn.execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE)").await;
        };
        match rt {
            Ok(runtime) => runtime.block_on(run),
            Err(_) => tracing::error!("VACUUM 运行时创建失败"),
        }
    });
}

// ── 查询/语言 ────────────────────────────────────────────────────

/// 词典测试查询：前缀匹配（不限启用状态），按 word_lower 排序
pub async fn test_query(
    db: &DatabaseConnection,
    dictionary_id: i32,
    word: &str,
    limit: i64,
) -> Result<Vec<Value>, AppError> {
    get_dictionary(db, dictionary_id).await?;
    let word_lower = word.trim().to_lowercase();
    let (lower, upper) = crate::services::entry_scope::word_lower_prefix_bounds(&word_lower);
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT e.id, e.word, e.phonetic, e.definition, e.extra FROM dict_entries e \
             JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
             WHERE e.dictionary_id = $1 AND e.word_lower >= $2 AND e.word_lower < $3 \
             ORDER BY e.word_lower LIMIT $4",
            [
                dictionary_id.into(),
                lower.into(),
                upper.into(),
                limit.into(),
            ],
        ))
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "word": row.try_get::<String>("", "word").unwrap_or_default(),
                "phonetic": row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                "definition": row.try_get::<String>("", "definition").unwrap_or_default(),
                "extra": row
                    .try_get::<Option<String>>("", "extra")
                    .ok()
                    .flatten()
                    .and_then(|s: String| serde_json::from_str::<Value>(&s).ok()),
            })
        })
        .collect())
}

/// 按当前采样逻辑重新识别一部词典的语言方向（只读源文件不写库）
pub async fn detect_dictionary_language(
    db: &DatabaseConnection,
    dictionary_id: i32,
) -> Result<(Option<String>, Option<String>), AppError> {
    let dictionary = get_dictionary(db, dictionary_id).await?;
    let paths = source_paths_for(db, dictionary_id).await?;
    if paths.is_empty() {
        return Err(AppError::validation(format!(
            "找不到「{}」的源文件，无法重新识别",
            dictionary.name
        )));
    }
    let format = dictionary.format;
    let detected = tokio::task::spawn_blocking(
        move || -> std::result::Result<_, dict_parser::ParserError> {
            let mut parser = parser_by_format(&format)?;
            let headwords = parser.sample_headwords(&paths, SAMPLE_HEADWORD_LIMIT)?;
            let sampled = parser.sample(&paths, SAMPLE_DEFINITION_LIMIT)?;
            Ok(detect_language(
                &headwords,
                &sampled.iter().map(|e| e.definition.clone()).collect::<Vec<_>>(),
            ))
        },
    )
    .await
    .map_err(|e| AppError::internal("spawn", e))?
    .map_err(|e| AppError::validation(e.to_string()))?;
    Ok((
        detected.0.map(String::from),
        detected.1.map(String::from),
    ))
}

/// 把重新识别出的语言方向写回；只覆盖识别出结论的那侧，返回是否有改动
pub async fn apply_detected_language(
    state: &AppState,
    dictionary_id: i32,
    detected_from: Option<&str>,
    detected_to: Option<&str>,
) -> Result<bool, AppError> {
    let dictionary = get_dictionary(&state.db, dictionary_id).await?;
    let mut changed = false;
    let (mut lang_from, mut lang_to) = (dictionary.lang_from.clone(), dictionary.lang_to.clone());
    if let Some(from) = detected_from {
        if lang_from != from {
            lang_from = from.to_string();
            changed = true;
        }
    }
    if let Some(to) = detected_to {
        if lang_to != to {
            lang_to = to.to_string();
            changed = true;
        }
    }
    if changed {
        state
            .db
            .execute_raw(Statement::from_sql_and_values(
                state.db.get_database_backend(),
                "UPDATE dictionaries SET lang_from = $1, lang_to = $2 WHERE id = $3",
                [lang_from.into(), lang_to.into(), dictionary_id.into()],
            ))
            .await?;
        state.query_cache.invalidate();
    }
    Ok(changed)
}

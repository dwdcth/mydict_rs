//! 修复历史遗留问题 —— 移植自 `app/services/definition_repair.py`。
//!
//! 三件事：①坏资源链接（早期改写 bug）就地替换 ②存量 `` `编号` `` 样式标记展开
//! ③牛津9 缺失美音例句喇叭清理。全部按主键区间分批、幂等、可中断重跑。

use regex::Regex;
use sea_orm::{ConnectionTrait, Statement};
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use dict_parser::mdict::expand_style_markers;

use crate::core::errors::AppError;
use crate::AppState;

/// 单批处理的主键区间大小：短事务不长占写锁，中断重跑只损失最后一批
pub const DEFAULT_BATCH_SIZE: i64 = 5000;

// ── 坏链接修复 ───────────────────────────────────────────────────

fn prefixes(dictionary_id: i32) -> (String, String, String) {
    let base = format!("/dict-res/{dictionary_id}/res/");
    (
        format!("{base}entry:/"),
        format!("{base}sound:/"),
        format!("{base}file:/"),
    )
}

/// 修复某部词典里全部坏链接（SQL replace 三连），返回实际改动行数。
/// 刻意不恢复 javascript: 与 //host、www.host（无法与真实资源路径区分）。
/// lite 词典释义不落库（源文件是唯一真相），修复类任务全部 no-op
async fn is_lite_dictionary(state: &AppState, dictionary_id: i32) -> Result<bool, AppError> {
    use sea_orm::ConnectionTrait;
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT entry_mode FROM dictionaries WHERE id = $1",
            [dictionary_id.into()],
        ))
        .await?;
    Ok(row
        .as_ref()
        .and_then(|r| r.try_get::<String>("", "entry_mode").ok())
        .map(|m| m == "lite")
        .unwrap_or(false))
}

pub async fn repair_legacy_links(
    state: &AppState,
    dictionary_id: i32,
    batch_size: i64,
    dry_run: bool,
) -> Result<i64, AppError> {
    if is_lite_dictionary(state, dictionary_id).await? {
        return Ok(0);
    }

    let (entry_from, sound_from, file_from) = prefixes(dictionary_id);
    let res_prefix = format!("/dict-res/{dictionary_id}/res/");
    let backend = state.db.get_database_backend();

    if dry_run {
        let row = state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT \
                    SUM(CASE WHEN definition LIKE '%' || $1 || '%' THEN 1 ELSE 0 END) AS e, \
                    SUM(CASE WHEN definition LIKE '%' || $2 || '%' THEN 1 ELSE 0 END) AS s, \
                    SUM(CASE WHEN definition LIKE '%' || $3 || '%' THEN 1 ELSE 0 END) AS f \
                 FROM dict_entries WHERE dictionary_id = $4",
                [
                    entry_from.clone().into(),
                    sound_from.clone().into(),
                    file_from.clone().into(),
                    dictionary_id.into(),
                ],
            ))
            .await?;
        let e: i64 = row.as_ref().and_then(|r| r.try_get("", "e").ok()).unwrap_or(0);
        let s: i64 = row.as_ref().and_then(|r| r.try_get("", "s").ok()).unwrap_or(0);
        let f: i64 = row.as_ref().and_then(|r| r.try_get("", "f").ok()).unwrap_or(0);
        return Ok(e + s + f);
    }

    // 三条替换互不包含，SQL 层一次 UPDATE 表达（SQLite/PG 的 replace 嵌套）
    let new_definition = format!(
        "replace(replace(replace(definition, '{file_from}', '{res_prefix}'), '{sound_from}', '{res_prefix}'), '{entry_from}', 'entry://')"
    );
    let row = state
        .db
        .query_one_raw(Statement::from_string(
            backend,
            format!(
                "SELECT MIN(id) AS lo, MAX(id) AS hi FROM dict_entries WHERE dictionary_id = {dictionary_id}"
            ),
        ))
        .await?;
    let (Some(lo), Some(hi)) = (
        row.as_ref().and_then(|r| r.try_get::<i64>("", "lo").ok()),
        row.as_ref().and_then(|r| r.try_get::<i64>("", "hi").ok()),
    ) else {
        return Ok(0);
    };

    let mut repaired: i64 = 0;
    let mut cursor = lo - 1;
    while cursor < hi {
        let window_end = std::cmp::min(cursor + batch_size, hi);
        let sql = format!(
            "UPDATE dict_entries SET definition = {new_definition} \
             WHERE dictionary_id + 0 = {dictionary_id} AND id > {cursor} AND id <= {window_end} \
               AND (definition LIKE '%{entry_from}%' OR definition LIKE '%{sound_from}%' OR definition LIKE '%{file_from}%')"
        );
        let result = state
            .db
            .execute_raw(Statement::from_string(backend, sql))
            .await?;
        repaired += result.rows_affected() as i64;
        cursor = window_end;
    }
    Ok(repaired)
}

// ── 样式标记展开 ─────────────────────────────────────────────────

/// 把已入库释义里的 `` `编号` `` 标记就地展开（读-转换-写回，带状态扫描不能 SQL 表达）。
/// 幂等：展开过的文本不再含标记。
pub async fn expand_stored_styles(
    state: &AppState,
    dictionary_id: i32,
    stylesheet: &HashMap<String, (String, String)>,
    compact: bool,
    batch_size: i64,
) -> Result<i64, AppError> {
    if is_lite_dictionary(state, dictionary_id).await? {
        return Ok(0);
    }

    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one_raw(Statement::from_string(
            backend,
            format!(
                "SELECT MIN(id) AS lo, MAX(id) AS hi FROM dict_entries WHERE dictionary_id = {dictionary_id}"
            ),
        ))
        .await?;
    let (Some(lo), Some(hi)) = (
        row.as_ref().and_then(|r| r.try_get::<i64>("", "lo").ok()),
        row.as_ref().and_then(|r| r.try_get::<i64>("", "hi").ok()),
    ) else {
        return Ok(0);
    };

    let mut changed_total: i64 = 0;
    let mut cursor = lo - 1;
    while cursor < hi {
        let window_end = std::cmp::min(cursor + batch_size, hi);
        // 只取含反引号的行（绝大多数词典一条都不含）
        let rows = state
            .db
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT id, definition FROM dict_entries \
                     WHERE id > {cursor} AND id <= {window_end} \
                       AND dictionary_id + 0 = {dictionary_id} AND definition LIKE '%`%'"
                ),
            ))
            .await?;
        let mut updates: Vec<(i64, String)> = Vec::new();
        for row in &rows {
            let id: i64 = row.try_get("", "id").unwrap_or_default();
            let definition: String = row.try_get("", "definition").unwrap_or_default();
            let expanded = expand_style_markers(&definition, stylesheet, compact);
            if expanded != definition {
                updates.push((id, expanded));
            }
        }
        for (id, new_definition) in &updates {
            state
                .db
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE dict_entries SET definition = $1 WHERE id = $2",
                    [new_definition.clone().into(), (*id).into()],
                ))
                .await?;
        }
        changed_total += updates.len() as i64;
        cursor = window_end;
    }
    Ok(changed_total)
}

/// 找出「词条里含反引号」的 MDict 词典（避免给全部词典开 .mdx——打开要读整份词头索引）
pub async fn dictionaries_using_style_markers(
    state: &AppState,
    dictionary_ids: &[i32],
) -> Result<Vec<i32>, AppError> {
    if dictionary_ids.is_empty() {
        return Ok(Vec::new());
    }
    let backend = state.db.get_database_backend();
    let ids: Vec<sea_orm::Value> = dictionary_ids.iter().map(|id| (*id).into()).collect();
    let ph = crate::services::query::sql_placeholders(backend, ids.len());
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT id FROM dictionaries WHERE id IN ({ph}) AND format = 'mdict'"),
            ids,
        ))
        .await?;
    let mut found = Vec::new();
    for row in rows {
        let id: i32 = row.try_get("", "id").unwrap_or_default();
        let hit = state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT id FROM dict_entries WHERE dictionary_id = $1 AND definition LIKE '%`%' LIMIT 1",
                [id.into()],
            ))
            .await?
            .is_some();
        if hit {
            found.push(id);
        }
    }
    Ok(found)
}

/// 从这部词典的 .mdx 源读 StyleSheet；读不到（源被删/LZO 打不开）返回空表 + false
pub fn source_style_context(
    sources: &[std::path::PathBuf],
) -> (HashMap<String, (String, String)>, bool) {
    
    for source in sources {
        let mut path = source.clone();
        if path.is_dir() {
            let mut mdx_files: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().map(|e| e.to_string_lossy().eq_ignore_ascii_case("mdx")).unwrap_or(false) {
                        mdx_files.push(p);
                    }
                }
            }
            mdx_files.sort();
            if mdx_files.is_empty() {
                continue;
            }
            path = mdx_files[0].clone();
        }
        let is_mdx = path
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("mdx"))
            .unwrap_or(false);
        if !is_mdx || !path.is_file() {
            continue;
        }
        // 用 probe 探测后交给 dict-parser 的打开逻辑成本高；这里轻量读头即可
        if let Some((sheet, compact)) = read_style_context_from_mdx(&path) {
            return (sheet, compact);
        }
    }
    (HashMap::new(), false)
}

/// 轻量读 .mdx 头的 StyleSheet/Compact（只读头几千字节，不加载词头索引）
fn read_style_context_from_mdx(path: &Path) -> Option<(HashMap<String, (String, String)>, bool)> {
    use dict_parser::mdict::parse_stylesheet;
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    let header_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if bytes.len() < 4 + header_len {
        return None;
    }
    let header_bytes = &bytes[4..4 + header_len];
    // 头可能是 UTF-16LE 或 UTF-8
    let text = if header_bytes.len() % 2 == 0 {
        let units: Vec<u16> = header_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(header_bytes).into_owned()
    };
    let extract_attr = |key: &str| -> Option<String> {
        let pattern = Regex::new(&format!(r#"(?i){key}\s*=\s*["']([^"']*)["']"#)).ok()?;
        pattern
            .captures(&text)
            .map(|c| c[1].to_string())
    };
    let sheet = parse_stylesheet(extract_attr("StyleSheet").as_deref());
    let compact = extract_attr("Compact")
        .map(|v| v.trim().to_lowercase() == "yes")
        .unwrap_or(false);
    Some((sheet, compact))
}

// ── 牛津9 红色美音例句喇叭清理 ────────────────────────────────────

static USS_ANCHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<a href="(?P<url>[^"]*uss[^"]*\.mp3)"><audio-uss-liju>[^<]*</audio-uss-liju></a>"#)
        .unwrap()
});

/// 删掉「指向缺失 mp3」的红色美音喇叭锚点，返回 (改动词条数, 删除喇叭数)。
/// 存在性走 /dict-res 完整口径（磁盘 res/ + .mdd 大小写不敏感兜底）——
/// 路由能取到的文件就不删按钮。正则回调不能 async，所以分三步：
/// 同步收集锚点 → 异步批量判存在 → 同步重建字符串。
pub async fn remove_missing_uss_speakers(
    state: &AppState,
    dictionary_id: i32,
    res_dir: &Path,
    batch_size: i64,
) -> Result<(i64, i64), AppError> {
    if is_lite_dictionary(state, dictionary_id).await? {
        return Ok((0, 0));
    }

    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one_raw(Statement::from_string(
            backend,
            format!(
                "SELECT MIN(id) AS lo, MAX(id) AS hi FROM dict_entries WHERE dictionary_id = {dictionary_id}"
            ),
        ))
        .await?;
    let (Some(lo), Some(hi)) = (
        row.as_ref().and_then(|r| r.try_get::<i64>("", "lo").ok()),
        row.as_ref().and_then(|r| r.try_get::<i64>("", "hi").ok()),
    ) else {
        return Ok((0, 0));
    };

    let mut entries_changed: i64 = 0;
    let mut anchors_removed: i64 = 0;
    let mut cursor = lo - 1;
    while cursor < hi {
        let window_end = std::cmp::min(cursor + batch_size, hi);
        let rows = state
            .db
            .query_all_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT id, definition FROM dict_entries \
                     WHERE id > {cursor} AND id <= {window_end} \
                       AND dictionary_id + 0 = {dictionary_id} \
                       AND definition LIKE '%audio-uss-liju%'"
                ),
            ))
            .await?;
        cursor = window_end;
        let mut updates: Vec<(i64, String)> = Vec::new();
        for row in &rows {
            let id: i64 = row.try_get("", "id").unwrap_or_default();
            let definition: String = row.try_get("", "definition").unwrap_or_default();
            // ① 同步收集：每个锚点 (相对路径, 原文, 区间)
            let mut anchors: Vec<(String, String, std::ops::Range<usize>)> = Vec::new();
            for caps in USS_ANCHOR_RE.captures_iter(&definition) {
                let whole = caps.get(0).expect("group 0");
                let url = caps.name("url").map(|m| m.as_str()).unwrap_or_default();
                let relative = url.rsplit("/res/").next().unwrap_or(url).to_string();
                anchors.push((relative, whole.as_str().to_string(), whole.range()));
            }
            if anchors.is_empty() {
                continue;
            }
            // ② 异步批量判存在（去重）
            let mut unique: Vec<String> = anchors
                .iter()
                .map(|(rel, _, _)| rel.clone())
                .collect();
            unique.sort();
            unique.dedup();
            let mut available: std::collections::HashMap<String, bool> =
                std::collections::HashMap::new();
            for rel in &unique {
                let exists = if dict_parser::resources::resolve_resource_file(res_dir, rel).is_some() {
                    true
                } else {
                    crate::services::mdd_resources::resource_exists(state, dictionary_id, rel).await
                };
                available.insert(rel.clone(), exists);
            }
            // ③ 同步重建：文件还在的恢复原文，缺失的删除
            let mut removed_here: i64 = 0;
            let mut cleaned = String::with_capacity(definition.len());
            let mut last = 0usize;
            for (rel, original, range) in &anchors {
                cleaned.push_str(&definition[last..range.start]);
                if *available.get(rel).unwrap_or(&false) {
                    cleaned.push_str(original); // 文件还在，按钮保留
                } else {
                    removed_here += 1;
                }
                last = range.end;
            }
            cleaned.push_str(&definition[last..]);
            if removed_here > 0 {
                updates.push((id, cleaned));
                anchors_removed += removed_here;
            }
        }
        for (id, new_definition) in &updates {
            state
                .db
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE dict_entries SET definition = $1 WHERE id = $2",
                    [new_definition.clone().into(), (*id).into()],
                ))
                .await?;
        }
        entries_changed += updates.len() as i64;
    }
    Ok((entries_changed, anchors_removed))
}

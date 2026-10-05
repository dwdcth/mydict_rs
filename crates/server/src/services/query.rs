//! 查询核心 —— 移植自 `app/services/query_service.py`。

use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::entities::dictionary;
use crate::services::entry_scope::word_lower_prefix_bounds;
use crate::services::query_expand::expand_word;
use crate::AppState;

/// 「精确未命中 → 前缀兜底」时每部词典最多返回的词头数
const PREFIX_FALLBACK_LIMIT: i64 = 8;
/// @@@LINK 解引用最大层数（防环）
const MAX_LINK_DEPTH: usize = 5;

const ZH_LANG_CODES: [&str; 3] = ["zh", "zh-Hans", "zh-Hant"];
const BLOCK_TAGS: [&str; 11] = ["p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6"];

// ── HTML → 纯文本（v1 /query full_style=false 用）────────────────

/// 块级标签起止各插一个换行；实体解码；行 strip、去空行。
/// 对齐 Python HTMLParser(convert_charrefs=True) 行为。
pub fn html_to_plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut entity_start: Option<usize> = None;
    let bytes: Vec<char> = html.chars().collect();
    let mut i = 0usize;
    let mut literal = String::new();

    macro_rules! flush_literal {
        () => {
            if !literal.is_empty() {
                text.push_str(&decode_entities(&literal));
                literal.clear();
            }
        };
    }

    while i < bytes.len() {
        let c = bytes[i];
        if c == '<' {
            flush_literal!();
            in_tag = true;
            tag_name.clear();
            i += 1;
            // 跳过 </ 的斜杠与标签名前的空白
            while i < bytes.len() && (bytes[i] == '/' || bytes[i].is_whitespace()) {
                i += 1;
            }
            continue;
        }
        if in_tag {
            if c.is_ascii_alphanumeric() {
                tag_name.push(c.to_ascii_lowercase());
            } else {
                // 标签名结束（遇空格、属性、> 等）；若是块级标签插换行。
                // 注意：纯扫描不解析属性值里的 '>'（真实词典释义没有这种写法，
                // Python 的 HTMLParser 同样按第一个 '>' 收尾）
                if !tag_name.is_empty() && BLOCK_TAGS.contains(&tag_name.as_str()) {
                    text.push('\n');
                }
                if c == '>' {
                    in_tag = false;
                }
                tag_name.clear();
            }
            i += 1;
            continue;
        }
        // 文本
        literal.push(c);
        let _ = entity_start.take();
        i += 1;
    }
    flush_literal!();
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    lines.join("\n")
}

/// 常见字符实体解码（html-escape crate 的 decode）
fn decode_entities(text: &str) -> String {
    html_escape::decode_html_entities(text).to_string()
}

// ── 语言识别（查询侧粗粒度版）────────────────────────────────────

pub fn detect_lang(word: &str) -> &'static str {
    // 含 CJK 表意文字（U+4E00-U+9FFF）即视为中文
    if word.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        "zh"
    } else {
        "en"
    }
}

/// 解析 `dict=a,b,c`；空串与无有效数字视为「不限制」，非法项忽略
pub fn parse_dict_ids(raw: Option<&str>) -> Option<Vec<i32>> {
    let raw = raw?;
    let ids: Vec<i32> = raw
        .split(',')
        .filter_map(|part| part.trim().parse::<i32>().ok())
        .filter(|id| *id > 0)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

fn lang_from_values(lang_from: &str) -> Vec<String> {
    if lang_from == "zh" {
        ZH_LANG_CODES.iter().map(|s| s.to_string()).collect()
    } else {
        vec![lang_from.to_string()]
    }
}

// ── 候选词典解析 ─────────────────────────────────────────────────

async fn enabled_dictionaries(
    db: &DatabaseConnection,
    allowed_ids: Option<&[i32]>,
) -> Result<Vec<dictionary::Model>, AppError> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
    let mut query = dictionary::Entity::find()
        .filter(dictionary::Column::Status.eq("enabled"));
    if let Some(allowed) = allowed_ids {
        query = query.filter(dictionary::Column::Id.is_in(allowed.to_vec()));
    }
    Ok(query
        .order_by_asc(dictionary::Column::SortOrder)
        .order_by_asc(dictionary::Column::Id)
        .all(db)
        .await?)
}

pub struct CandidateSet {
    pub dictionaries: Vec<dictionary::Model>,
    pub preferred_ids: HashSet<i32>,
}

/// 解析候选词典，标出「语言方向与输入一致」的那批。
/// lang_from 自动识别不可靠（实测 63 部里 6 部中文词典被判成 en），所以分
/// 「优先/其余」两批：优先语言没命中再退到其余语言，判错的词典仍可达。
#[allow(clippy::too_many_arguments)]
pub async fn resolve_candidates(
    db: &DatabaseConnection,
    word: &str,
    dict_ids: Option<&[i32]>,
    lang_from: Option<&str>,
    lang_to: Option<&str>,
    allowed_ids: Option<&[i32]>,
    all_langs: bool,
) -> Result<CandidateSet, AppError> {
    let rows = enabled_dictionaries(db, allowed_ids).await?;
    let rows = match dict_ids {
        Some(ids) if !ids.is_empty() => {
            let idset: HashSet<i32> = ids.iter().copied().collect();
            rows.into_iter().filter(|d| idset.contains(&d.id)).collect()
        }
        _ => rows,
    };
    if let Some(lang_from) = lang_from.filter(|s| !s.is_empty()) {
        let wanted = lang_from_values(lang_from);
        let mut scoped: Vec<dictionary::Model> = rows
            .into_iter()
            .filter(|d| wanted.contains(&d.lang_from))
            .collect();
        if let Some(lang_to) = lang_to.filter(|s| !s.is_empty()) {
            scoped.retain(|d| d.lang_to == lang_to);
        }
        let ids: HashSet<i32> = scoped.iter().map(|d| d.id).collect();
        return Ok(CandidateSet {
            dictionaries: scoped,
            preferred_ids: ids,
        });
    }
    if dict_ids.is_some() {
        let ids: HashSet<i32> = rows.iter().map(|d| d.id).collect();
        return Ok(CandidateSet {
            dictionaries: rows,
            preferred_ids: ids,
        });
    }
    // all_langs：多语言模式（阅读器「全部语言」标签），全部候选一视同仁
    if all_langs {
        let ids: HashSet<i32> = rows.iter().map(|d| d.id).collect();
        return Ok(CandidateSet {
            dictionaries: rows,
            preferred_ids: ids,
        });
    }
    let wanted = lang_from_values(detect_lang(word));
    let mut preferred = Vec::new();
    let mut others = Vec::new();
    for d in rows {
        if wanted.contains(&d.lang_from) {
            preferred.push(d);
        } else {
            others.push(d);
        }
    }
    let preferred_ids: HashSet<i32> = preferred.iter().map(|d| d.id).collect();
    preferred.extend(others);
    Ok(CandidateSet {
        dictionaries: preferred,
        preferred_ids,
    })
}

/// 只返回「优先语言」那批词典（前缀建议、收藏挑词典等场景）
pub async fn resolve_dictionaries(
    db: &DatabaseConnection,
    word: &str,
    dict_ids: Option<&[i32]>,
    lang_from: Option<&str>,
    lang_to: Option<&str>,
    allowed_ids: Option<&[i32]>,
) -> Result<Vec<dictionary::Model>, AppError> {
    let candidates =
        resolve_candidates(db, word, dict_ids, lang_from, lang_to, allowed_ids, false).await?;
    Ok(candidates
        .dictionaries
        .into_iter()
        .filter(|d| candidates.preferred_ids.contains(&d.id))
        .collect())
}

pub async fn list_public_dictionaries(
    db: &DatabaseConnection,
    allowed_ids: Option<&[i32]>,
) -> Result<Vec<dictionary::Model>, AppError> {
    enabled_dictionaries(db, allowed_ids).await
}

// ── 词条行 ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EntryRow {
    pub id: i32,
    pub dictionary_id: i32,
    pub word: String,
    pub word_lower: String,
    pub phonetic: Option<String>,
    pub definition: String,
    pub extra: Option<String>,
    /// lite 远程行：源文件内定位（NULL = 全量行，definition 已落库）
    pub source_ordinal: Option<i64>,
}

/// 生成 count 个占位符（PG 用 $N 从 start 开始、SQLite 用 ?）——动态 IN 列表用
pub fn sql_placeholders(backend: sea_orm::DbBackend, count: usize) -> String {
    (1..=count)
        .map(|i| match backend {
            sea_orm::DbBackend::Postgres => format!("${i}"),
            _ => "?".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 占位符发生器：按 SQL 文本中的出现顺序取号（PG 用 $N、SQLite 用 ?），
/// 杜绝参数序与占位符序错位。
struct Ph {
    backend: sea_orm::DbBackend,
    next: usize,
}

impl Ph {
    fn new(backend: sea_orm::DbBackend) -> Self {
        Self { backend, next: 1 }
    }
    fn take(&mut self) -> String {
        let n = self.next;
        self.next += 1;
        match self.backend {
            sea_orm::DbBackend::Postgres => format!("${n}"),
            _ => "?".to_string(),
        }
    }
    fn take_n(&mut self, count: usize) -> String {
        (0..count).map(|_| self.take()).collect::<Vec<_>>().join(",")
    }
}

async fn query_entries(
    db: &DatabaseConnection,
    words_lower: &[String],
    dictionary_ids: &[i32],
) -> Result<Vec<EntryRow>, AppError> {
    if dictionary_ids.is_empty() || words_lower.is_empty() {
        return Ok(Vec::new());
    }
    let backend = db.get_database_backend();
    let mut ph = Ph::new(backend);
    let dict_ph = ph.take_n(dictionary_ids.len());
    let word_ph = ph.take_n(words_lower.len());
    let sql = format!(
        "SELECT e.id, e.dictionary_id, e.word, e.word_lower, e.phonetic, e.definition, e.extra, e.source_ordinal \
         FROM dict_entries e JOIN dictionaries d \
           ON e.dictionary_id = d.id AND e.generation = d.active_generation \
         WHERE e.dictionary_id IN ({dict_ph}) AND e.word_lower IN ({word_ph})"
    );
    let mut values: Vec<sea_orm::Value> = Vec::with_capacity(dictionary_ids.len() + words_lower.len());
    for id in dictionary_ids {
        values.push((*id).into());
    }
    for w in words_lower {
        values.push(w.clone().into());
    }
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
        .await?;
    Ok(rows
        .iter()
        .map(|row| EntryRow {
            id: row.try_get("", "id").unwrap_or_default(),
            dictionary_id: row.try_get("", "dictionary_id").unwrap_or_default(),
            word: row.try_get("", "word").unwrap_or_default(),
            word_lower: row.try_get::<Option<String>>("", "word_lower").ok().flatten().unwrap_or_default(),
            phonetic: row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
            definition: row.try_get("", "definition").unwrap_or_default(),
            extra: row.try_get::<Option<String>>("", "extra").ok().flatten(),
            source_ordinal: row.try_get::<Option<i64>>("", "source_ordinal").ok().flatten(),
        })
        .collect())
}

async fn prefix_fallback_entries(
    db: &DatabaseConnection,
    prefix_lower: &str,
    dictionary_id: i32,
) -> Result<Vec<EntryRow>, AppError> {
    if prefix_lower.is_empty() {
        return Ok(Vec::new());
    }
    let (lower, upper) = word_lower_prefix_bounds(prefix_lower);
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT e.id, e.dictionary_id, e.word, e.word_lower, e.phonetic, e.definition, e.extra, e.source_ordinal \
             FROM dict_entries e JOIN dictionaries d \
               ON e.dictionary_id = d.id AND e.generation = d.active_generation \
             WHERE e.dictionary_id = $1 AND e.word_lower >= $2 AND e.word_lower < $3 \
             ORDER BY e.word_lower LIMIT $4",
            [
                dictionary_id.into(),
                lower.into(),
                upper.into(),
                PREFIX_FALLBACK_LIMIT.into(),
            ],
        ))
        .await?;
    Ok(rows
        .iter()
        .map(|row| EntryRow {
            id: row.try_get("", "id").unwrap_or_default(),
            dictionary_id: row.try_get("", "dictionary_id").unwrap_or_default(),
            word: row.try_get("", "word").unwrap_or_default(),
            word_lower: row.try_get::<Option<String>>("", "word_lower").ok().flatten().unwrap_or_default(),
            phonetic: row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
            definition: row.try_get("", "definition").unwrap_or_default(),
            extra: row.try_get::<Option<String>>("", "extra").ok().flatten(),
            source_ordinal: row.try_get::<Option<i64>>("", "source_ordinal").ok().flatten(),
        })
        .collect())
}

// ── @@@LINK 解引用 ───────────────────────────────────────────────

/// 释义是 `@@@LINK=xxx` 时返回目标词头（大小写不敏感），否则 None
fn link_target_ci(definition: &str) -> Option<String> {
    let trimmed = definition.trim_start();
    let lower = trimmed.to_lowercase();
    let rest = lower.strip_prefix("@@@link")?;
    let _ = rest;
    // 大小写不敏感匹配 @@@LINK=，但保留原文大小写取目标
    let idx = trimmed.find("=")?;
    let prefix_ok = trimmed[..idx]
        .trim()
        .eq_ignore_ascii_case("@@@LINK")
        || trimmed[..idx].trim() == "@@@LINK";
    if !prefix_ok {
        return None;
    }
    // @@@LINK 与 = 之间不能有其它字符
    let between = &trimmed[..idx];
    if between.trim_end().len() != "@@@LINK".len() {
        return None;
    }
    let target = trimmed[idx + 1..].trim();
    (!target.is_empty()).then(|| target.to_string())
}

/// 跟进词条重定向，返回真正承载释义的那条记录。
/// 只在本词典内跳转；链式最多 MAX_LINK_DEPTH 层；目标缺失返回当前条（显示标记比空白好排查）。
/// lite 远程行（definition 为空 + source_ordinal 存在）先从源文件物化释义——
/// 既为 @@@LINK 判定，也把最终承载行的 definition 填成完整内容（出口即物化）。
async fn resolve_link(state: &AppState, entry: &EntryRow) -> Result<EntryRow, AppError> {
    let db = &state.db;
    let mut current = entry.clone();
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..MAX_LINK_DEPTH {
        if current.definition.is_empty() {
            if let Some(ordinal) = current.source_ordinal {
                if let Some(def) = crate::services::mdx_resources::materialize_definition(
                    state, current.dictionary_id, ordinal,
                )
                .await
                {
                    current.definition = (*def).clone();
                }
            }
        }
        let Some(target) = link_target_ci(&current.definition) else {
            return Ok(current);
        };
        let key = target.to_lowercase();
        if seen.contains(&key) {
            return Ok(current);
        }
        seen.insert(key.clone());
        let following = query_entries(db, &[key], &[entry.dictionary_id])
            .await?
            .into_iter()
            .next();
        match following {
            Some(following) => current = following,
            None => return Ok(entry.clone()),
        }
    }
    Ok(current)
}

/// 公开版：生词本等落库路径复用同一套解引用口径
pub async fn resolve_entry_link(
    state: &AppState,
    entry: &EntryRow,
) -> Result<EntryRow, AppError> {
    resolve_link(state, entry).await
}

/// 把可能是 @@@LINK= 的释义解引用成真正承载内容的释义（生词本老快照兜底）
pub async fn resolve_link_definition(
    state: &AppState,
    dictionary_id: Option<i32>,
    definition: Option<&str>,
) -> Result<Option<String>, AppError> {
    let db = &state.db;
    let Some(dictionary_id) = dictionary_id else {
        return Ok(definition.map(String::from));
    };
    let Some(mut current) = definition.map(String::from) else {
        return Ok(None);
    };
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..MAX_LINK_DEPTH {
        let Some(target) = link_target_ci(&current) else {
            return Ok(Some(current));
        };
        let key = target.to_lowercase();
        if seen.contains(&key) {
            return Ok(Some(current));
        }
        seen.insert(key.clone());
        let following = query_entries(db, &[key], &[dictionary_id])
            .await?
            .into_iter()
            .next();
        match following {
            Some(following) => {
                // lite 远程目标行：先物化再判定
                if following.definition.is_empty() {
                    if let Some(ordinal) = following.source_ordinal {
                        if let Some(def) = crate::services::mdx_resources::materialize_definition(
                            state, following.dictionary_id, ordinal,
                        )
                        .await
                        {
                            current = (*def).clone();
                            continue;
                        }
                    }
                }
                current = following.definition;
            }
            None => return Ok(Some(current)),
        }
    }
    Ok(Some(current))
}

// ── 查词 ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn search_word(
    state: &AppState,
    word: &str,
    dict_ids: Option<&[i32]>,
    lang_from: Option<&str>,
    lang_to: Option<&str>,
    allowed_ids: Option<&[i32]>,
    include_definitions: bool,
    all_langs: bool,
) -> Result<Value, AppError> {
    let candidates = resolve_candidates(
        &state.db, word, dict_ids, lang_from, lang_to, allowed_ids, all_langs,
    )
    .await?;
    if candidates.dictionaries.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let word_lower = word.trim().to_lowercase();
    let variants = expand_word(word);
    // 缓存 key 用完整候选集：只按「优先语言」做 key 的话，空结果会挡掉兜底路径
    let cache_key = crate::core::query_cache::QueryCache::make_key(
        &word_lower,
        &candidates.dictionaries.iter().map(|d| d.id).collect::<Vec<_>>(),
        include_definitions,
        all_langs,
    );
    if let Some(cached) = state.query_cache.get(&cache_key) {
        return Ok((*cached).clone());
    }

    let by_id: std::collections::HashMap<i32, &dictionary::Model> = candidates
        .dictionaries
        .iter()
        .map(|d| (d.id, d))
        .collect();
    let preferred: Vec<i32> = candidates
        .dictionaries
        .iter()
        .filter(|d| candidates.preferred_ids.contains(&d.id))
        .map(|d| d.id)
        .collect();
    let others: Vec<i32> = candidates
        .dictionaries
        .iter()
        .filter(|d| !candidates.preferred_ids.contains(&d.id))
        .map(|d| d.id)
        .collect();

    let mut entries = query_entries(&state.db, &variants, &preferred).await?;
    let fallback_scope: Vec<i32> = if !entries.is_empty() {
        // 优先语言有精确命中：其余语言完全不参与（既不精确、也不前缀）
        preferred.clone()
    } else {
        let other_entries = query_entries(&state.db, &variants, &others).await?;
        let others_hit = !other_entries.is_empty();
        entries = other_entries;
        if others_hit {
            others.clone()
        } else {
            let mut all = preferred.clone();
            all.extend(&others);
            all
        }
    };

    // 前缀兜底：fallback_scope 里精确未命中的词典退回前缀匹配
    let hit_ids: HashSet<i32> = entries.iter().map(|e| e.dictionary_id).collect();
    let mut prefix_entries = Vec::new();
    for candidate in &candidates.dictionaries {
        if hit_ids.contains(&candidate.id) || !fallback_scope.contains(&candidate.id) {
            continue;
        }
        prefix_entries.extend(prefix_fallback_entries(&state.db, &word_lower, candidate.id).await?);
    }
    entries.extend(prefix_entries);

    // 结果顺序决定前端手风琴哪部默认展开：按候选词典顺序排
    let order: std::collections::HashMap<i32, usize> = candidates
        .dictionaries
        .iter()
        .enumerate()
        .map(|(index, d)| (d.id, index))
        .collect();
    let mut results: Vec<(usize, Value)> = Vec::new();
    let mut seen_targets: HashSet<(i32, i32)> = HashSet::new();
    for e in &entries {
        // @@@LINK 跟进到目标取内容，词头仍显示用户查到的那个
        let resolved = resolve_link(state, e).await?;
        // 同词典几个变体跳到同一目标只留一条
        let target = (e.dictionary_id, resolved.id);
        if seen_targets.contains(&target) {
            continue;
        }
        seen_targets.insert(target);
        let dict = *by_id.get(&e.dictionary_id).expect("candidate");
        let mut item = json!({
            "id": e.id,
            "dictionary_id": e.dictionary_id,
            "dictionary_name": dict.name,
            "word": e.word,
            "phonetic": resolved.phonetic,
            "lang_from": dict.lang_from,
            "extra": e.extra.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            "lang_match": candidates.preferred_ids.contains(&e.dictionary_id),
        });
        if include_definitions {
            item["definition"] = json!(resolved.definition);
        }
        results.push((*order.get(&e.dictionary_id).expect("ordered"), item));
    }
    results.sort_by_key(|(order, _)| *order);
    let array = Value::Array(results.into_iter().map(|(_, item)| item).collect());
    let arc = std::sync::Arc::new(array.clone());
    state.query_cache.insert(cache_key, arc);
    Ok(array)
}

/// 取某部词典里的一条词条，**不做 @@@LINK 解引用**（生词本存原词头用）
pub async fn get_raw_entry(
    db: &DatabaseConnection,
    dictionary_id: i32,
    word: &str,
) -> Result<Option<EntryRow>, AppError> {
    let word_lower = word.trim().to_lowercase();
    Ok(query_entries(db, &[word_lower], &[dictionary_id])
        .await?
        .into_iter()
        .next())
}

/// 取某部词典里的一条词条（词条渲染用），@@@LINK 已解引用
pub async fn get_entry(
    state: &AppState,
    dictionary_id: i32,
    word: &str,
) -> Result<Option<EntryRow>, AppError> {
    let word_lower = word.trim().to_lowercase();
    let entry = query_entries(&state.db, &[word_lower], &[dictionary_id])
        .await?
        .into_iter()
        .next();
    match entry {
        Some(entry) => Ok(Some(resolve_link(state, &entry).await?)),
        None => Ok(None),
    }
}

/// 取某部词典里与这个词匹配的**全部**词条（词条文档聚合用，同名词头多条合一）。
/// entry_ids 是客户端输入，只认 search 可能返回的那些（变体集 ∪ 前缀兜底 id）；
/// 归属校验放应用侧——`dictionary_id=? AND id IN(…)` 会让规划器放弃主键。
pub async fn get_entries_for_document(
    state: &AppState,
    dictionary_id: i32,
    word: &str,
    entry_ids: Option<&[i32]>,
) -> Result<Vec<EntryRow>, AppError> {
    let db = &state.db;
    let word_lower = word.trim().to_lowercase();
    if word_lower.is_empty() {
        return Ok(Vec::new());
    }
    let variants = expand_word(word);
    let variants_lower: HashSet<String> = variants.iter().cloned().collect();
    let exact = query_entries(db, &variants, &[dictionary_id]).await?;

    let prefix_ids: HashSet<i32> = if exact.is_empty() {
        prefix_fallback_entries(db, &word_lower, dictionary_id)
            .await?
            .into_iter()
            .map(|e| e.id)
            .collect()
    } else {
        HashSet::new()
    };

    let authorized = |entry: &EntryRow| -> bool {
        if entry.dictionary_id != dictionary_id {
            return false;
        }
        if variants_lower.contains(&entry.word_lower) {
            return true;
        }
        prefix_ids.contains(&entry.id)
    };

    let mut entries: Vec<EntryRow> = Vec::new();
    if let Some(ids) = entry_ids.filter(|ids| !ids.is_empty()) {
        let backend = db.get_database_backend();
        let mut ph = Ph::new(backend);
        let id_ph = ph.take_n(ids.len());
        let sql = format!(
            "SELECT e.id, e.dictionary_id, e.word, e.word_lower, e.phonetic, e.definition, e.extra, e.source_ordinal \
             FROM dict_entries e JOIN dictionaries d \
               ON e.dictionary_id = d.id AND e.generation = d.active_generation \
             WHERE e.id IN ({id_ph}) ORDER BY e.id"
        );
        let values: Vec<sea_orm::Value> = ids.iter().map(|id| (*id).into()).collect();
        let rows = db
            .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
            .await?;
        let candidates: Vec<EntryRow> = rows
            .iter()
            .map(|row| EntryRow {
                id: row.try_get("", "id").unwrap_or_default(),
                dictionary_id: row.try_get("", "dictionary_id").unwrap_or_default(),
                word: row.try_get("", "word").unwrap_or_default(),
                word_lower: row.try_get::<Option<String>>("", "word_lower").ok().flatten().unwrap_or_default(),
                phonetic: row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                definition: row.try_get("", "definition").unwrap_or_default(),
                extra: row.try_get::<Option<String>>("", "extra").ok().flatten(),
                source_ordinal: row.try_get::<Option<i64>>("", "source_ordinal").ok().flatten(),
            })
            .collect();
        entries = candidates.into_iter().filter(|e| authorized(e)).collect();
    }
    if entries.is_empty() {
        // 没传 id / 全都不在授权范围（重解析后 id 换新）→ 退回按词取，再退前缀
        entries = if !exact.is_empty() {
            exact
        } else {
            prefix_fallback_entries(db, &word_lower, dictionary_id).await?
        };
    }
    // 几条跳到同一个目标的只留一份
    let mut resolved: Vec<(i32, EntryRow)> = Vec::new();
    let mut seen: HashSet<i32> = HashSet::new();
    for entry in &entries {
        let target = resolve_link(state, entry).await?;
        if seen.insert(target.id) {
            resolved.push((target.id, target));
        }
    }
    resolved.sort_by_key(|(id, _)| *id);
    Ok(resolved.into_iter().map(|(_, e)| e).collect())
}

/// 前缀联想：候选词典前缀区间读 limit*3 行，去重后截断
pub async fn suggest_prefix(
    db: &DatabaseConnection,
    prefix: &str,
    dict_ids: Option<&[i32]>,
    limit: usize,
    allowed_ids: Option<&[i32]>,
) -> Result<Vec<String>, AppError> {
    let dictionaries =
        resolve_dictionaries(db, prefix, dict_ids, None, None, allowed_ids).await?;
    if dictionaries.is_empty() {
        return Ok(Vec::new());
    }
    let prefix_lower = prefix.trim().to_lowercase();
    let (lower, upper) = word_lower_prefix_bounds(&prefix_lower);
    let ids: Vec<i32> = dictionaries.iter().map(|d| d.id).collect();
    let backend = db.get_database_backend();
    let mut ph = Ph::new(backend);
    let lower_ph = ph.take();
    let upper_ph = ph.take();
    let in_ph = ph.take_n(ids.len());
    let limit_ph = ph.take();
    let sql = format!(
        "SELECT e.word FROM dict_entries e JOIN dictionaries d \
           ON e.dictionary_id = d.id AND e.generation = d.active_generation \
         WHERE e.word_lower >= {lower_ph} AND e.word_lower < {upper_ph} \
           AND e.dictionary_id IN ({in_ph}) \
         ORDER BY e.word_lower LIMIT {limit_ph}"
    );
    let mut values: Vec<sea_orm::Value> = vec![
        lower.into(),
        upper.into(),
    ];
    for id in &ids {
        values.push((*id).into());
    }
    values.push(((limit * 3) as i64).into());
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
        .await?;
    let mut seen = HashSet::new();
    let mut words = Vec::new();
    for row in rows {
        let w: String = row.try_get("", "word").unwrap_or_default();
        if seen.contains(&w) {
            continue;
        }
        seen.insert(w.clone());
        words.push(w);
        if words.len() >= limit {
            break;
        }
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_strips_blocks_and_decodes() {
        let html = "<div><p>Hello&nbsp;world</p><ul><li>a</li><li>b</li></ul></div>";
        // 与 Python 一致：行中的 &nbsp; 解码为 U+00A0 保留（只在行首尾 strip 时去掉）
        assert_eq!(html_to_plain_text(html), "Hello\u{a0}world\na\nb");
    }

    #[test]
    fn plain_text_preserves_line_semantics() {
        // Python 用例：full_style=false → "豫章\n江西省的别称。"
        let html = "<p>豫章</p><p>江西省的别称。</p>";
        assert_eq!(html_to_plain_text(html), "豫章\n江西省的别称。");
        assert_eq!(html_to_plain_text("plain &amp; simple"), "plain & simple");
        assert_eq!(html_to_plain_text(""), "");
    }

    #[test]
    fn link_target_parsing() {
        assert_eq!(link_target_ci("@@@LINK=apple"), Some("apple".into()));
        assert_eq!(link_target_ci("  @@@link=apple  "), Some("apple".into()));
        assert_eq!(link_target_ci("@@@LINK = 目标词 "), Some("目标词".into()));
        assert_eq!(link_target_ci("normal definition"), None);
        assert_eq!(link_target_ci(""), None);
    }

    #[test]
    fn lang_detection() {
        assert_eq!(detect_lang("苹果"), "zh");
        assert_eq!(detect_lang("中国【ちゅうごく①】"), "zh");
        assert_eq!(detect_lang("apple"), "en");
    }

    #[test]
    fn dict_ids_parsing() {
        assert_eq!(parse_dict_ids(Some("1,2,3")), Some(vec![1, 2, 3]));
        assert_eq!(parse_dict_ids(Some("1, x, 3")), Some(vec![1, 3]));
        assert_eq!(parse_dict_ids(Some("a,b")), None);
        assert_eq!(parse_dict_ids(Some("")), None);
        assert_eq!(parse_dict_ids(None), None);
    }
}

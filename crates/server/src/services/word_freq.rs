//! 文本词频分析（吸收自 PythonMDict 的 analyzer 思路）：
//! 贴入/上传英文文本（.txt/.epub）→ 分词 → 停用词过滤 → 用已导入词典验证「真实单词」
//! → 标记已在生词本/复习计划 → 按词频排序返回，前端勾选批量收藏。

use std::collections::{HashMap, HashSet};

use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::AppState;

/// 返回的词表上限（再高频也没意义，前 500 足够筛生词）
const MAX_WORDS: usize = 500;
/// IN 查询分块（SQLite 参数上限安全侧）
const IN_CHUNK: usize = 100;

fn is_stopword(w: &str) -> bool {
    const STOP: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "if", "of", "to", "in", "on", "at", "for", "with",
        "by", "from", "as", "is", "are", "was", "were", "be", "been", "being", "it", "its",
        "this", "that", "these", "those", "he", "she", "they", "them", "his", "her", "their",
        "we", "you", "your", "our", "i", "me", "my", "not", "no", "nor", "so", "too", "very",
        "can", "will", "just", "don", "should", "now", "than", "then", "there", "here", "when",
        "where", "why", "how", "all", "any", "both", "each", "few", "more", "most", "other",
        "some", "such", "only", "own", "same", "s", "t", "d", "ll", "m", "o", "re", "ve", "y",
        "ain", "aren", "couldn", "didn", "doesn", "hadn", "hasn", "haven", "isn", "ma", "mightn",
        "mustn", "needn", "shan", "shouldn", "wasn", "weren", "won", "wouldn", "about", "into",
        "over", "under", "again", "further", "once", "between", "during", "before", "after",
        "above", "below", "up", "down", "out", "off", "because", "while", "until", "against",
        "what", "which", "who", "whom", "am", "have", "has", "had", "having", "do", "does",
        "did", "doing", "would", "could", "ought", "get", "got", "also", "however", "one",
        "two", "three", "first", "second", "new", "like", "time", "make", "made", "said",
        "say", "says", "us", "itself", "himself", "herself", "themselves",
    ];
    STOP.contains(&w)
}

/// 分词：连续英文字母（含撇号/连字符），小写化；2-30 字符
fn tokenize(text: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut current = String::new();
    let flush = |current: &mut String, counts: &mut HashMap<String, usize>| {
        if current.len() >= 2 && current.len() <= 30 && !is_stopword(current) {
            *counts.entry(current.clone()).or_insert(0) += 1;
        }
        current.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphabetic() || ((c == '\'' || c == '-') && !current.is_empty()) {
            current.push(c.to_ascii_lowercase());
        } else {
            flush(&mut current, &mut counts);
        }
    }
    flush(&mut current, &mut counts);
    counts
}

/// epub（zip 容器）：抽出 xhtml/html 正文并剥标签。返回 None = 不是合法 zip
pub fn extract_epub_text(bytes: &[u8]) -> Option<String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;
    let mut out = String::new();
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_lowercase();
        if !(name.ends_with(".xhtml") || name.ends_with(".html") || name.ends_with(".htm")) {
            continue;
        }
        let mut html = String::new();
        if std::io::Read::read_to_string(&mut entry, &mut html).is_ok() {
            out.push_str(&crate::services::query::html_to_plain_text(&html));
            out.push('\n');
        }
    }
    Some(out)
}

/// 主流程：text 已是纯文本。返回按词频排序的词表（含收录/生词/复习标记）
pub async fn analyze(
    state: &AppState,
    user_id: Option<i32>,
    text: &str,
) -> Result<Value, AppError> {
    let counts = tokenize(text);
    let total_tokens: usize = counts.values().sum();
    if counts.is_empty() {
        return Ok(json!({
            "total_tokens": 0, "unique_words": 0, "words": [],
            "in_dict_count": 0, "in_vocab_count": 0,
        }));
    }
    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(MAX_WORDS);

    // 词典收录验证：启用词典当前代的 word_lower 精确命中（分块 IN）
    let backend = state.db.get_database_backend();
    let mut in_dict: HashSet<String> = HashSet::new();
    for chunk in ranked.chunks(IN_CHUNK) {
        let placeholders = crate::services::query::sql_placeholders(backend, chunk.len());
        let values: Vec<sea_orm::Value> =
            chunk.iter().map(|(w, _)| w.as_str().into()).collect();
        let rows = state
            .db
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT DISTINCT e.word_lower FROM dict_entries e \
                     JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
                     WHERE d.status = 'enabled' AND e.word_lower IN ({placeholders})"
                ),
                values,
            ))
            .await
            .map_err(AppError::from)?;
        for row in rows {
            if let Some(w) = row.try_get::<String>("", "word_lower").ok() {
                in_dict.insert(w);
            }
        }
    }

    // 用户的生词本/复习集
    let (mut in_vocab, mut in_review): (HashSet<String>, HashSet<String>) = (HashSet::new(), HashSet::new());
    if let Some(user_id) = user_id {
        let rows = state
            .db
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT v.word, EXISTS(SELECT 1 FROM flashcards f WHERE f.vocab_item_id = v.id) AS in_review \
                 FROM vocab_items v WHERE v.user_id = $1",
                [user_id.into()],
            ))
            .await
            .map_err(AppError::from)?;
        for row in rows {
            if let Some(w) = row.try_get::<String>("", "word").ok() {
                let lw = w.to_lowercase();
                if row.try_get::<Option<i32>>("", "in_review").ok().flatten().unwrap_or(0) != 0 {
                    in_review.insert(lw.clone());
                }
                in_vocab.insert(lw);
            }
        }
    }

    let words: Vec<Value> = ranked
        .iter()
        .map(|(w, n)| {
            json!({
                "word": w,
                "count": n,
                "in_dict": in_dict.contains(w),
                "in_vocab": in_vocab.contains(w),
                "in_review": in_review.contains(w),
            })
        })
        .collect();
    Ok(json!({
        "total_tokens": total_tokens,
        "unique_words": counts_len(&ranked),
        "words": words,
        "in_dict_count": words.iter().filter(|w| w["in_dict"] == true).count(),
        "in_vocab_count": words.iter().filter(|w| w["in_vocab"] == true).count(),
    }))
}

fn counts_len(ranked: &[(String, usize)]) -> usize {
    ranked.len()
}

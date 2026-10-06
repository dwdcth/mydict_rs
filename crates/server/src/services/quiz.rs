//! 例句挖空测验（吸收自 PythonMDict 的 quiz_worker 思路，Rust 重实现）：
//! 从生词本快照释义里抽含目标词的英文例句 → 挖空 → 同词典长度相近词做干扰项
//! → 4 选 1。答对/答错直接映射 FSRS 评分（Good/Again），测验即复习。

use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::AppState;

/// 干扰项筛选时的英语停用词（截自 PythonMDict 的表，够用即可）
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "of", "to", "in", "on", "at", "for", "with", "by",
    "from", "as", "is", "are", "was", "were", "be", "been", "being", "it", "its", "this", "that",
    "these", "those", "he", "she", "they", "them", "his", "her", "their", "we", "you", "your",
    "our", "i", "me", "my", "not", "no", "do", "does", "did", "have", "has", "had", "will",
    "would", "can", "could", "should", "shall", "may", "might", "must", "than", "then", "so",
    "such", "there", "here", "when", "where", "which", "who", "whom", "what", "how", "all",
    "any", "both", "each", "few", "more", "most", "other", "some", "only", "own", "same", "too",
    "very", "just", "also", "one", "two", "up", "down", "out", "about", "into", "over", "under",
];

/// 是否「无信息量」的词条头行（圈号义项/上标注释/IPA 音标/词性连排开头）
fn looks_like_entry_head(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return true;
    }
    // 圈号义项 ❶❷… 与「①②」
    if t.starts_with(|c: char| ('①'..='⑳').contains(&c) || ('❶'..='❿').contains(&c)) {
        return true;
    }
    // IPA 音标行 /…/
    if t.starts_with('/') && t.ends_with('/') {
        return true;
    }
    // 词性连排开头（n. / vt. / adj. 等，且很短）
    let lower = t.to_lowercase();
    if (lower.starts_with("n.")
        || lower.starts_with("v.")
        || lower.starts_with("vt.")
        || lower.starts_with("vi.")
        || lower.starts_with("adj.")
        || lower.starts_with("adv.")
        || lower.starts_with("prep.")
        || lower.starts_with("conj.")
        || lower.starts_with("pron.")
        || lower.starts_with("interj."))
        && t.chars().count() <= 24
    {
        return true;
    }
    // 纯数字/符号（页码、编号类词头）
    if !t.chars().any(|c| c.is_alphabetic()) {
        return true;
    }
    false
}

/// 句子切分：按 .!? 后随空白/结尾断句（英文例句足够；引号内缩写误切可接受）
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        current.push(*c);
        if matches!(c, '.' | '!' | '?') {
            let next = chars.get(i + 1);
            if next.is_none() || next.is_some_and(|n| n.is_whitespace()) {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                current.clear();
            }
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    out
}

/// 把句子里的目标词换成空格下划线（词边界、大小写不敏感），返回 (挖空句, 命中数)
fn cloze_sentence(sentence: &str, word: &str) -> Option<(String, usize)> {
    let word_chars: Vec<char> = word.to_lowercase().chars().collect();
    let chars: Vec<char> = sentence.chars().collect();
    let is_word_char = |c: char| c.is_alphanumeric() || c == '\'';
    let mut result = String::with_capacity(sentence.len());
    let mut hits = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        // 逐字符小写比对目标词
        let mut matched = i + word_chars.len() <= chars.len();
        if matched {
            for (j, wc) in word_chars.iter().enumerate() {
                let sc = chars[i + j].to_lowercase().next().unwrap_or('\0');
                let tc = wc.to_lowercase().next().unwrap_or('\0');
                if sc != tc {
                    matched = false;
                    break;
                }
            }
        }
        // 词边界：前后不能紧贴字母/数字/撇号
        let boundary_ok = matched && {
            let before_ok = i == 0 || !is_word_char(chars[i - 1]);
            let after = i + word_chars.len();
            let after_ok = after >= chars.len() || !is_word_char(chars[after]);
            before_ok && after_ok
        };
        if boundary_ok {
            result.push_str("____");
            hits += 1;
            i += word_chars.len();
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    (hits > 0).then(|| (result, hits))
}

/// 抽一条可挖空的例句：含目标词、长度适中、英文字符占比过半、剔除词条头行
fn pick_sentence(definition_html: &str, word: &str) -> Option<(String, usize)> {
    let plain = crate::services::query::html_to_plain_text(definition_html);
    for line in plain.lines() {
        if looks_like_entry_head(line) {
            continue;
        }
        for sentence in split_sentences(line) {
            let len = sentence.chars().count();
            if !(12..=160).contains(&len) {
                continue;
            }
            // 英文字符占比 > 50%（测验面向英语例句；中文释义行自然被排除）
            let alpha = sentence.chars().filter(|c| c.is_ascii_alphabetic()).count();
            if alpha * 2 <= len {
                continue;
            }
            if let Some((clozed, hits)) = cloze_sentence(&sentence, word) {
                return Some((clozed, hits));
            }
        }
    }
    None
}

/// 同词典长度相近的干扰项（±4、纯字母、非停用词、非答案本身）
async fn distractors(
    db: &sea_orm::DatabaseConnection,
    dictionary_id: i32,
    word: &str,
    count: usize,
) -> Result<Vec<String>, AppError> {
    // 长度在 Rust 侧算好传数值：SQLite 对 TEXT 参数做 `- 4` 会强转成 0，BETWEEN 全落空
    let word_len = word.chars().count() as i64;
    let backend = db.get_database_backend();
    let mut ph = crate::services::query::PhPub::new(backend);
    let d_ph = ph.take();
    let lo_ph = ph.take();
    let hi_ph = ph.take();
    // 注：不加纯字母的 SQL 过滤（GLOB 是 SQLite 方言）；候选 24 条在 Rust 侧筛
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT DISTINCT e.word FROM dict_entries e \
                 JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
                 WHERE e.dictionary_id = {d_ph} \
                   AND length(e.word) BETWEEN {lo_ph} AND {hi_ph} \
                 ORDER BY RANDOM() LIMIT 24"
            ),
            [dictionary_id.into(), (word_len - 4).into(), (word_len + 4).into()],
        ))
        .await
        .map_err(AppError::from)?;
    let lower_word = word.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        let candidate = row.try_get::<String>("", "word").unwrap_or_default();
        let lc = candidate.to_lowercase();
        if lc == lower_word
            || STOPWORDS.contains(&lc.as_str())
            || candidate.len() < 2
            || !candidate.chars().all(|c| c.is_ascii_alphabetic())
        {
            continue;
        }
        if out.iter().any(|w| w.to_lowercase() == lc) {
            continue;
        }
        out.push(candidate);
        if out.len() >= count {
            break;
        }
    }
    Ok(out)
}

/// 生成测验：优先取到期卡，不足再从全部卡随机补；抽不到例句/干扰项的卡跳过
pub async fn build_quiz(
    state: &AppState,
    user_id: i32,
    count: i64,
) -> Result<Value, AppError> {
    let backend = state.db.get_database_backend();
    let now = chrono::Utc::now().timestamp();
    // 到期在前（due asc），新卡按序，不足 count 就全取
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT f.vocab_item_id, v.word, v.definition, v.dictionary_id, d.name AS dictionary_name \
             FROM flashcards f \
             JOIN vocab_items v ON v.id = f.vocab_item_id \
             LEFT JOIN dictionaries d ON d.id = v.dictionary_id \
             WHERE v.user_id = $1 \
             ORDER BY (f.due <= $2) DESC, f.due, f.vocab_item_id \
             LIMIT $3",
            [user_id.into(), now.into(), (count * 3).into()],
        ))
        .await
        .map_err(AppError::from)?;

    let mut questions: Vec<Value> = Vec::new();
    for row in &rows {
        if questions.len() as i64 >= count {
            break;
        }
        let word = row.try_get::<String>("", "word").unwrap_or_default();
        let definition = row
            .try_get::<Option<String>>("", "definition")
            .ok()
            .flatten()
            .unwrap_or_default();
        let dictionary_id = row
            .try_get::<Option<i32>>("", "dictionary_id")
            .ok()
            .flatten();
        let Some((sentence, hits)) = pick_sentence(&definition, &word) else {
            continue;
        };
        let Some(dictionary_id) = dictionary_id else {
            continue;
        };
        let mut options = distractors(&state.db, dictionary_id, &word, 3).await?;
        if options.len() < 3 {
            continue; // 词典太小凑不齐干扰项，跳过这张卡
        }
        options.push(word.clone());
        // 洗牌（时间作种，会话内足够）
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize + questions.len() * 7919)
            .unwrap_or(42);
        shuffle(&mut options, seed);
        let correct_index = options
            .iter()
            .position(|o| o.eq_ignore_ascii_case(&word))
            .unwrap_or(0);
        questions.push(json!({
            "vocab_item_id": row.try_get::<i32>("", "vocab_item_id").unwrap_or_default(),
            "sentence": sentence,
            "blank_count": hits,
            "options": options,
            "correct_index": correct_index,
            "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
        }));
    }
    Ok(json!({ "questions": questions }))
}

fn shuffle<T>(items: &mut [T], seed: usize) {
    // xorshift 简易洗牌（无需 rand 依赖）
    let mut state = (seed as u64).max(1);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in (1..items.len()).rev() {
        let j = (next() as usize) % (i + 1);
        items.swap(i, j);
    }
}

/// 测验作答：答对=Good(3)、答错=Again(1)，直接走 FSRS 复习更新
pub async fn answer(
    state: &AppState,
    user_id: i32,
    vocab_item_id: i32,
    correct: bool,
) -> Result<Value, AppError> {
    let rating = if correct { 3 } else { 1 };
    let mut result =
        crate::services::flashcards::review_card(state, user_id, vocab_item_id, rating).await?;
    if let Some(obj) = result.as_object_mut() {
        obj.insert("correct".into(), json!(correct));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_sentence_finds_apple() {
        let def = "n. 苹果<br>The apple is a sweet fruit that grows on trees.";
        let picked = pick_sentence(def, "apple");
        assert!(picked.is_some(), "应能抽出例句");
        let (sentence, hits) = picked.unwrap();
        assert!(sentence.contains("____"));
        assert_eq!(hits, 1);

        // 词条头行应被跳过
        assert!(looks_like_entry_head("n. 苹果"));
        assert!(looks_like_entry_head("①古郡名"));
        assert!(!looks_like_entry_head("The apple is sweet."));
    }
}

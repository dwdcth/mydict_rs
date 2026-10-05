//! 统一词条结构与采样辅助 —— 移植自 Python 版 `app/parsers/base.py`。

use regex::Regex;
use std::sync::LazyLock;

/// 释义可能是 HTML（MDict 的释义整段是 HTML），标签名与属性名全是拉丁字母。
/// 「判断这段文字用什么语言写」「这段是不是根本没有正文」之前都必须先把标签剥掉。
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// 字符实体同样要剥掉：`&nbsp;` 会被当成 4 个拉丁字母算进占比，中文释义里密集出现的
/// 话足以把语言判断从 zh 拉向 en。
static ENTITY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&(?:[a-zA-Z][a-zA-Z0-9]{1,7}|#[0-9]{1,7}|#[xX][0-9a-fA-F]{1,6});").unwrap()
});

/// 释义剥掉标签与实体后少于这个字符数就认为「没有正文」——扫描版词典（整页是 `<img>`）
/// 属于这类，拿它们去判语言只会得到噪声。阈值取 2 而不是更大：真实词典里
/// `n. 苹果` 这种极短释义是合法内容，不能误伤。
const MIN_DEFINITION_CHARS: usize = 2;

/// 采样释义时最多扫过的条数 = limit * 本系数。开头可能全是索引项或整页扫描图（全被判为
/// 无信息量），所以要多扫一些；但不能无限扫，否则采样就成了整部词典的解析。
pub const SAMPLE_SCAN_FACTOR: usize = 25;

/// 统一词条结构，跨格式解析器共用。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub word: String,
    pub definition: String,
    pub phonetic: Option<String>,
    /// 格式特有的结构化元数据（ECDICT 的 tag/collins/oxford/exchange/frq、
    /// StarDict 的 alias_of 等）；None = 无。
    pub extra: Option<serde_json::Value>,
}

impl ParsedEntry {
    pub fn new(word: impl Into<String>, definition: impl Into<String>) -> Self {
        Self {
            word: word.into(),
            definition: definition.into(),
            phonetic: None,
            extra: None,
        }
    }
}

/// 去掉 HTML 标签与字符实体，只留下真正的文字。
pub fn strip_markup(text: Option<&str>) -> String {
    let Some(text) = text else { return String::new() };
    if text.is_empty() {
        return String::new();
    }
    let no_tags = TAG_RE.replace_all(text, " ");
    ENTITY_RE.replace_all(&no_tags, " ").into_owned()
}

/// 词头里至少要有一个「有意义的文字字符」：字母、汉字（含扩展区）或假名。
/// 纯数字/符号的词条（`0`、`110`、`---`）是索引项，不代表词典正文用什么语言写。
/// 汉典的前若干条正是 `0`、`110`、`120` 这种，只看开头就会把整部中文词典判成 en。
pub fn is_informative_headword(word: &str) -> bool {
    word.chars().any(is_meaningful_char)
}

fn is_meaningful_char(c: char) -> bool {
    matches!(c, 'A'..='Z' | 'a'..='z')
        || matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
        || matches!(c, '\u{3040}'..='\u{30ff}')
}

/// 这条词条是否适合用来判断词典的语言方向。
///
/// 采样只取「开头若干条」时，开头往往是索引页、数字条目或整页扫描图，据此判断会判错；
/// 这里把这类无信息量的样本剔掉，剩下的才参与判定。
pub fn is_informative(word: &str, definition: Option<&str>) -> bool {
    if !is_informative_headword(word) {
        return false;
    }
    strip_markup(definition).trim().chars().count() >= MIN_DEFINITION_CHARS
}

/// 把「已按跨度遍历整份词典收集到的候选」再均匀降到 limit 个。
///
/// 为什么不能「收满 limit 就停」：那样收集到的是遍历顺序里最靠前的那批，等于又退回
/// 「只取开头」。必须先跨完整份收集、再统一下采样，才真的覆盖词典全段。
pub fn spread_downsample(candidates: Vec<String>, limit: usize) -> Vec<String> {
    if candidates.len() <= limit {
        return candidates;
    }
    let step = std::cmp::max(1, candidates.len() / limit);
    candidates
        .into_iter()
        .step_by(step)
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_markup_removes_tags_and_entities() {
        assert_eq!(strip_markup(Some("<b>hello</b>")), " hello ");
        assert_eq!(strip_markup(Some("a&nbsp;b")), "a b");
        assert_eq!(strip_markup(Some("&#65;&#x42;")), "  ");
        assert_eq!(strip_markup(None), "");
        assert_eq!(strip_markup(Some("")), "");
    }

    #[test]
    fn informative_headword_filters_index_items() {
        assert!(is_informative_headword("apple"));
        assert!(is_informative_headword("苹果"));
        assert!(is_informative_headword("あ【亜】"));
        assert!(!is_informative_headword("0"));
        assert!(!is_informative_headword("110"));
        assert!(!is_informative_headword("---"));
    }

    #[test]
    fn informative_requires_min_definition_chars() {
        assert!(is_informative("苹果", Some("n. 水果")));
        assert!(!is_informative("苹果", Some("a")));
        assert!(!is_informative("苹果", Some("<img src=\"x.png\"/>")));
        assert!(!is_informative("0", Some("n. long definition here")));
    }

    #[test]
    fn downsample_spreads_across_whole_list() {
        let candidates: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let out = spread_downsample(candidates, 10);
        assert_eq!(out.len(), 10);
        assert_eq!(out[0], "0");
        assert_eq!(out[9], "90");
        // 不满 limit 原样返回
        let small = vec!["a".to_string(), "b".to_string()];
        assert_eq!(spread_downsample(small.clone(), 5), small);
    }
}

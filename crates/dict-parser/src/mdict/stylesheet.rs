//! 展开 MDict 的 `` `编号` `` 样式标记 —— 移植自 `app/parsers/mdict_stylesheet.py`。
//!
//! `.mdx` 头部 `StyleSheet` 字段每个编号占三行（编号/开始标签/结束标记）。正文里写
//! `` `编号` `` 引用。展开规则：**遇到 `` `N` `` 时，先补上一个标记的结束标记，再输出 N
//! 的开始标签**，并把 N 的结束标记记为待补；文末补上最后那个待补的结束标记。
//!
//! 门控：反引号标记只在 mdx 头部 `Compact=Yes` 时才有意义。非 Compact 的词典里反引号
//! 数字是巧合文本，一个字节都不动。Compact 但样式表为空时标记直接剔除；编号没定义时
//! 也剔除——MDict 客户端不会把标记原样显示。

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// `` `12` `` 这种标记；\d+ 贪婪匹配，所以 `12` 不会被拆成 `1` + 2`
static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`(\d+)`").unwrap());

/// 把 `StyleSheet` 字段解析成 `{编号: (开始标签, 结束标记)}`。
///
/// 格式是每个编号占三行，但真实词典里偶尔夹着空行，所以按「哪一行是纯数字」定位，
/// 遇到畸形数据时只少几条规则，不会越界抛异常。空字段返回空表。
pub fn parse_stylesheet(raw: Option<&str>) -> HashMap<String, (String, String)> {
    let mut sheet = HashMap::new();
    let Some(raw) = raw else { return sheet };
    if raw.is_empty() {
        return sheet;
    }
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut index = 0;
    while index < lines.len() {
        let number = lines[index].trim();
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            index += 1;
            continue;
        }
        let begin = lines.get(index + 1).map(|s| s.trim().to_string()).unwrap_or_default();
        let end = lines.get(index + 2).map(|s| s.trim().to_string()).unwrap_or_default();
        sheet.insert(number.to_string(), (begin, end));
        index += 3;
    }
    sheet
}

/// 把正文里的 `` `编号` `` 展开成对应的 HTML 标签。
pub fn expand_style_markers(
    text: &str,
    sheet: &HashMap<String, (String, String)>,
    compact: bool,
) -> String {
    if text.is_empty() || !compact || !text.contains('`') {
        return text.to_string();
    }
    if sheet.is_empty() {
        return MARKER_RE.replace_all(text, "").into_owned();
    }

    let mut parts: Vec<String> = Vec::new();
    let mut pending = String::new();
    let mut last = 0usize;
    for mat in MARKER_RE.find_iter(text) {
        let number = &mat.as_str()[1..mat.as_str().len() - 1];
        parts.push(text[last..mat.start()].to_string());
        parts.push(pending.clone());
        pending.clear();
        if let Some((begin, end)) = sheet.get(number) {
            parts.push(begin.clone());
            pending = end.clone();
        }
        last = mat.end();
    }
    parts.push(text[last..].to_string());
    parts.push(pending);
    parts.concat()
}

/// Compact 判定：头部 `Compact=Yes`（值 strip 后 lower == "yes"）
pub fn is_compact_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().to_lowercase() == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> HashMap<String, (String, String)> {
        let raw = "1\n<b><center><font size=5 color=Green>\n</font></center></b><hr>\n2\n<br>\n";
        parse_stylesheet(Some(raw))
    }

    #[test]
    fn parse_tolerates_blank_lines() {
        let s = parse_stylesheet(Some("\n1\n<b>\n</b>\n\n2\n<i>\n</i>\n"));
        assert_eq!(s.len(), 2);
        assert_eq!(s["1"], ("<b>".to_string(), "</b>".to_string()));
        assert_eq!(s["2"], ("<i>".to_string(), "</i>".to_string()));
        assert!(parse_stylesheet(None).is_empty());
        assert!(parse_stylesheet(Some("")).is_empty());
    }

    #[test]
    fn non_compact_text_untouched() {
        let s = sheet();
        assert_eq!(
            expand_style_markers("`1`不`2``7`◆不`10`bù ㄅㄨˋ`2`", &s, false),
            "`1`不`2``7`◆不`10`bù ㄅㄨˋ`2`"
        );
    }

    #[test]
    fn compact_with_empty_sheet_strips_markers() {
        assert_eq!(expand_style_markers("a`1`b`99`c", &HashMap::new(), true), "abc");
    }

    #[test]
    fn expansion_closes_previous_marker_first() {
        let s = sheet();
        // `1` 开 → `2` 前先补 `1` 的结束标记 → 文末补 `2` 的结束标记
        assert_eq!(
            expand_style_markers("A`1`B`2`C", &s, true),
            "A<b><center><font size=5 color=Green>B</font></center></b><hr><br>C"
        );
    }

    #[test]
    fn undefined_marker_stripped_in_compact() {
        let s = sheet();
        // `7`/`10` 未定义 → 剔除（保持 pending 不变）
        assert_eq!(
            expand_style_markers("`7`x`1`y", &s, true),
            "x<b><center><font size=5 color=Green>y</font></center></b><hr>"
        );
    }

    #[test]
    fn greedy_number_matching() {
        let mut s = HashMap::new();
        s.insert("12".to_string(), ("<t12>".to_string(), "</t12>".to_string()));
        assert_eq!(expand_style_markers("`12`a", &s, true), "<t12>a</t12>");
    }

    #[test]
    fn idempotent_on_expanded_text() {
        let s = sheet();
        let once = expand_style_markers("A`1`B", &s, true);
        // 展开结果不再含反引号标记，二次展开不变
        assert_eq!(expand_style_markers(&once, &s, true), once);
    }
}

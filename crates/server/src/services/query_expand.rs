//! 查询词变体扩展（繁简、全角/半角）—— 移植自 `app/services/query_expand.py`。
//!
//! 词典收录哪种写法不统一（有的只收繁体、有的只收全角），用户输入取决于习惯与
//! 输入法。这里只做「同一条词条的不同写法」这一层，不做模糊匹配。

use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// 繁简转换是「一对多」的（发 → 發/髮），且不同地区用字不同，多取几个配置一起用
fn opencc_config(index: usize) -> opencc_rs::Config {
    match index {
        0 => opencc_rs::Config::T2S,
        1 => opencc_rs::Config::S2T,
        2 => opencc_rs::Config::S2TW,
        _ => opencc_rs::Config::S2HK,
    }
}

/// 变体数上限：再多基本是重复，却会让 SQL 的 IN 列表与缓存 key 无谓变长
const MAX_VARIANTS: usize = 16;

/// ASCII 可见字符与全角形式之间的固定码点差
const FULLWIDTH_OFFSET: u32 = 0xFEE0;

struct OpenCcSet([opencc_rs::OpenCC; 4]);

static CONVERTERS: OnceLock<OpenCcSet> = OnceLock::new();

fn converters() -> &'static OpenCcSet {
    CONVERTERS.get_or_init(|| {
        let make = |index: usize| {
            opencc_rs::OpenCC::new([opencc_config(index)]).expect("OpenCC 初始化")
        };
        OpenCcSet([make(0), make(1), make(2), make(3)])
    })
}

/// 把 ASCII 可见字符转成全角（用户输入半角、词典里存的是全角时的兜底）
fn to_fullwidth(text: &str) -> String {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            if (0x21..0x7F).contains(&code) {
                char::from_u32(code + FULLWIDTH_OFFSET).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// NFKC 归一：全角 ASCII 收成半角，半角片假名（ﾊﾝｶｸ）连浊点一起合成常规写法
fn normalize_width(text: &str) -> String {
    text.nfkc().collect()
}

/// 返回该词的全部查询变体，含原词、已去重、已小写。第一个元素一定是原词。
pub fn expand_word(word: &str) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let add = |value: Option<String>, variants: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        let Some(value) = value else { return };
        if variants.len() >= MAX_VARIANTS {
            return;
        }
        let key = value.trim().to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            return;
        }
        seen.insert(key.clone());
        variants.push(key);
    };

    // 先取形态变体（原样 / 宽度归一 / 全角），再对每种形态取繁简变体
    let bases = [
        word.to_string(),
        normalize_width(word),
        to_fullwidth(word),
    ];
    for base in &bases {
        add(Some(base.clone()), &mut variants, &mut seen);
        for converter in &converters().0 {
            // 转换失败只是少一个变体，不该影响查询
            if let Ok(converted) = converter.convert(base) {
                add(Some(converted), &mut variants, &mut seen);
            }
        }
    }
    variants
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_word_first() {
        let v = expand_word("Apple");
        assert_eq!(v[0], "apple");
        assert!(v.iter().all(|x| x == &x.to_lowercase()));
        assert!(v.len() <= 16);
    }

    #[test]
    fn simplified_traditional_both_present() {
        // 简体 → 繁体变体
        let v = expand_word("头发");
        assert!(v.contains(&"頭髮".to_string()), "variants: {v:?}");
        // 繁体 → 简体变体
        let v2 = expand_word("頭髮");
        assert!(v2.contains(&"头发".to_string()), "variants: {v2:?}");
        assert_eq!(v2[0], "頭髮");
    }

    #[test]
    fn fullwidth_and_nfkc_variants() {
        // 半角 → 全角变体
        let v = expand_word("abc123");
        assert!(v.contains(&"ａｂｃ１２３".to_string()), "variants: {v:?}");
        // 全角输入 → 半角变体（NFKC）
        let v2 = expand_word("ａｂｃ");
        assert!(v2.contains(&"abc".to_string()), "variants: {v2:?}");
    }

    #[test]
    fn dedup_and_limit() {
        let v = expand_word("a");
        let mut unique = v.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), v.len(), "无重复");
    }

    #[test]
    fn empty_word_gives_no_variants() {
        assert!(expand_word("").is_empty());
        assert!(expand_word("  ").is_empty());
    }
}

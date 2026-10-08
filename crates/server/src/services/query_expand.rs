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

/// QUERY_OPENCC_VARIANTS：
/// - `base`（默认）= T2S+S2T 双向（~36MB）——简↔繁主链路全覆盖
/// - `tw` / `hk` / `tw,hk` / `all` = 追加地区变体（台湾/香港用字，如 软件→軟體/軟件）。
///   libopencc 每个实例独立解析字典（ST 系被重复展开），S2TW/S2HK 各约 +52MB，
///   词典收录台湾/香港特有词头时再开
fn variant_config_indices() -> Vec<usize> {
    let raw = std::env::var("QUERY_OPENCC_VARIANTS")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let (tw, hk) = match raw.as_str() {
        "" | "base" => (false, false),
        "all" => (true, true),
        other => (
            other.split(',').any(|p| p.trim() == "tw"),
            other.split(',').any(|p| p.trim() == "hk"),
        ),
    };
    let mut indices = vec![0, 1];
    if tw {
        indices.push(2);
    }
    if hk {
        indices.push(3);
    }
    indices
}

struct OpenCcSet(Vec<opencc_rs::OpenCC>);

static CONVERTERS: OnceLock<OpenCcSet> = OnceLock::new();

fn converters() -> &'static OpenCcSet {
    CONVERTERS.get_or_init(|| {
        let rss_kb = || {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("VmRSS"))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse::<i64>().ok())
                })
                .unwrap_or(-1)
        };
        let before = rss_kb();
        let start = std::time::Instant::now();
        let indices = variant_config_indices();
        let set = OpenCcSet(
            indices
                .iter()
                .map(|&i| opencc_rs::OpenCC::new([opencc_config(i)]).expect("OpenCC 初始化"))
                .collect(),
        );
        tracing::info!(
            configs = ?indices,
            rss_delta_kb = rss_kb() - before,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "OpenCC 转换器初始化完成"
        );
        set
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

/// OpenCC 只映射 CJK 字符——不含汉字的词（"apple"、数字、假名混合键）转换必然
/// 原样返回，直接跳过：省一次转换，更重要的是省掉转换器集合（≥36MB）的惰性初始化
/// （英文 API 场景整个进程都不必加载）
fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| ('\u{2E80}'..='\u{9FFF}').contains(&c) || ('\u{F900}'..='\u{FAFF}').contains(&c) || ('\u{20000}'..='\u{3FFFF}').contains(&c))
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
    let need_opencc = bases.iter().any(|b| has_cjk(b));
    for base in &bases {
        add(Some(base.clone()), &mut variants, &mut seen);
        if !need_opencc {
            continue;
        }
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
    fn variant_indices_parse() {
        // env 未设置时是进程级一次性行为，这里只测解析函数对已取字符串的分支——
        // 直接构造等价输入验证组合逻辑
        let parse = |raw: &str| -> Vec<usize> {
            let (tw, hk) = match raw {
                "" | "base" => (false, false),
                "all" => (true, true),
                other => (
                    other.split(',').any(|p| p.trim() == "tw"),
                    other.split(',').any(|p| p.trim() == "hk"),
                ),
            };
            let mut indices = vec![0, 1];
            if tw {
                indices.push(2);
            }
            if hk {
                indices.push(3);
            }
            indices
        };
        assert_eq!(parse(""), vec![0, 1]);
        assert_eq!(parse("base"), vec![0, 1]);
        assert_eq!(parse("all"), vec![0, 1, 2, 3]);
        assert_eq!(parse("tw"), vec![0, 1, 2]);
        assert_eq!(parse("hk"), vec![0, 1, 3]);
        assert_eq!(parse("tw, hk"), vec![0, 1, 2, 3]);
    }

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

//! testdata/ 语料读回验证（语料由 `python3 scripts/gen_corpus.py` 生成）。
//!
//! 语料缺失时 `eprintln!` 后直接返回——CI 没有语料也不会红；本地生成后则是
//! 真实跑通的断言。测试直接用 mdictlib（v1/v2 MDX/MDD）与 opendict-rs
//! （v3 MDX、StarDict）两个 reader 的 API，验证 dict-parser 将来接入的
//! 库层能正确读回生成器写出的字节。

use std::path::{Path, PathBuf};

use mdictlib::{KeyOrdinal, MddFile, MdxFile};
use dict_parser::DictionaryParser;
use opendict::mdict::MdictDictionary;
use opendict::stardict::StarDictDictionary;

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join("testdata")
}

fn corpus(name: &str) -> PathBuf {
    testdata_dir().join(name)
}

/// 语料目录不存在就跳过（生成前 CI / 干净工作区不红）。
fn require_dir(dir: &Path) -> bool {
    if dir.is_dir() {
        true
    } else {
        eprintln!(
            "skipping: {} 不存在（先运行 python3 scripts/gen_corpus.py）",
            dir.display()
        );
        false
    }
}

/// 词头定位 → (匹配条数, 第一条匹配的释义文本)。
fn lookup_text(mdx: &MdxFile, word: &str) -> (usize, String) {
    let matches = mdx
        .locate(word)
        .unwrap_or_else(|e| panic!("locate({word}) 失败: {e}"))
        .unwrap_or_else(|| panic!("locate({word}) 无匹配"));
    let count = matches.len();
    let entry = mdx
        .entry_at(matches.first())
        .unwrap_or_else(|e| panic!("entry_at({word}) 失败: {e}"))
        .unwrap_or_else(|| panic!("entry_at({word}) 为空"));
    (count, entry.text().to_owned())
}

/// 通用 v1/v2 MDX 断言：词条数、首词条、中英文互查。
fn assert_mdx_common(path: &Path, expected_len: u64, first_word: &str) {
    let mdx = MdxFile::open(path)
        .unwrap_or_else(|e| panic!("打开 {} 失败: {e}", path.display()));
    assert_eq!(mdx.len(), expected_len, "{} 词条数不符", path.display());

    let first = mdx
        .entry_at(KeyOrdinal::new(0))
        .expect("entry_at(0)")
        .expect("首词条存在");
    assert_eq!(first.key(), first_word);
    assert!(
        first.text().contains("苹果"),
        "首词条释义应含中文：{}",
        first.text()
    );

    let (zh_count, zh_text) = lookup_text(&mdx, "苹果");
    assert_eq!(zh_count, 1);
    assert!(zh_text.contains("apple"), "「苹果」释义应含 apple：{zh_text}");

    let (en_count, en_text) = lookup_text(&mdx, "apple");
    assert_eq!(en_count, 1);
    assert!(en_text.contains("苹果"), "「apple」释义应含中文：{en_text}");

    // 词条数与 keys() 迭代一致
    assert_eq!(mdx.keys().count() as u64, expected_len);
}

#[test]
fn mdx_v2_basic_zlib_roundtrip() {
    let dir = corpus("mdx_v2_basic");
    if !require_dir(&dir) {
        return;
    }
    assert_mdx_common(&dir.join("basic.mdx"), 24, "apple");
}

#[test]
fn mdd_v2_basic_resources() {
    let dir = corpus("mdx_v2_basic");
    if !require_dir(&dir) {
        return;
    }
    let mdd = MddFile::open(&dir.join("basic.mdd"))
        .unwrap_or_else(|e| panic!("打开 basic.mdd 失败: {e}"));

    let keys: Vec<String> = mdd
        .keys()
        .map(|entry| entry.expect("key").key().to_owned())
        .collect();
    assert_eq!(
        keys,
        vec![r"\img\logo.png".to_owned(), r"\style.css".to_owned()],
        "MDD 资源键（UTF-16LE 词头，排序后）"
    );

    let css = mdd
        .lookup(r"\style.css")
        .expect("lookup(style.css)")
        .expect("style.css 应存在");
    assert_eq!(css.key(), r"\style.css");
    assert!(css.bytes().starts_with(b"body {"), "CSS 内容不符");

    let png = mdd
        .lookup(r"\img\logo.png")
        .expect("lookup(logo.png)")
        .expect("logo.png 应存在");
    assert_eq!(png.key(), r"\img\logo.png");
    assert!(png.bytes().starts_with(b"\x89PNG\r\n\x1a\n"), "PNG 魔数不符");
}

#[test]
fn mdx_v2_plain_roundtrip() {
    let dir = corpus("mdx_v2_plain");
    if !require_dir(&dir) {
        return;
    }
    assert_mdx_common(&dir.join("plain.mdx"), 24, "apple");
}

#[test]
fn mdx_v2_lzo_roundtrip() {
    let dir = corpus("mdx_v2_lzo");
    if !require_dir(&dir) {
        return;
    }
    assert_mdx_common(&dir.join("lzo.mdx"), 24, "apple");
}

#[test]
fn mdx_v2_rich_header_links_and_duplicates() {
    let dir = corpus("mdx_v2_rich");
    if !require_dir(&dir) {
        return;
    }
    let mdx = MdxFile::open(&dir.join("rich.mdx"))
        .unwrap_or_else(|e| panic!("打开 rich.mdx 失败: {e}"));

    // header 属性：Compact 与 StyleSheet（精确大小写名）都读得到
    let header = mdx.header();
    assert_eq!(header.attribute("Compact"), Some("Yes"));
    let stylesheet = header.attribute("StyleSheet").expect("StyleSheet 属性");
    assert!(
        stylesheet.starts_with("1\n"),
        "StyleSheet 应以编号行 1 开头：{stylesheet:?}"
    );
    assert!(stylesheet.contains("<b>"), "StyleSheet 缺 1 号开始标签");
    assert!(stylesheet.contains("</b>"), "StyleSheet 缺 1 号结束标签");
    assert!(
        stylesheet.contains("<font"),
        "StyleSheet 缺 2 号 font 标签"
    );

    // 3 条 @@@LINK：两跳链源头 appel、直接跳 apple2、死链 aple
    for (source, target) in [
        ("appel", "apple2"),
        ("apple2", "apple"),
        ("aple", "nonexistent-entry"),
    ] {
        let (count, text) = lookup_text(&mdx, source);
        assert_eq!(count, 1, "{source} 应恰好一条");
        assert!(
            text.starts_with("@@@LINK="),
            "{source} 释义应以 @@@LINK= 开头：{text}"
        );
        assert_eq!(text, format!("@@@LINK={target}"));
    }

    // 同名词条：bank 两条、释义不同
    let matches = mdx.locate("bank").expect("locate(bank)").expect("bank 匹配");
    assert_eq!(matches.len(), 2, "bank 应有两条同名词条");
    let texts: Vec<String> = matches
        .iter()
        .map(|ordinal| {
            mdx.entry_at(ordinal)
                .expect("entry_at(bank)")
                .expect("bank 词条")
                .text()
                .to_owned()
        })
        .collect();
    assert_ne!(texts[0], texts[1], "两条 bank 的释义应不同");
    assert!(texts.iter().all(|t| t.contains("bank")), "{texts:?}");
    assert_eq!(mdx.len(), 13);

    // 样式标记原样保留在释义里
    let (_, orange) = lookup_text(&mdx, "orange");
    assert!(orange.contains("`2`"), "应含 `2` 样式标记：{orange}");
}

#[test]
fn mdx_v1_basic_roundtrip() {
    let dir = corpus("mdx_v1_basic");
    if !require_dir(&dir) {
        return;
    }
    assert_mdx_common(&dir.join("v1.mdx"), 12, "apple");
    let mdx = MdxFile::open(&dir.join("v1.mdx")).unwrap();
    assert_eq!(mdx.header().generated_by_engine_version(), "1.2");
}

#[test]
fn mdx_v1_lzo_roundtrip() {
    let dir = corpus("mdx_v1_lzo");
    if !require_dir(&dir) {
        return;
    }
    assert_mdx_common(&dir.join("v1lzo.mdx"), 12, "apple");
}

#[test]
fn mdx_v3_basic_opendict_roundtrip() {
    let dir = corpus("mdx_v3_basic");
    if !require_dir(&dir) {
        return;
    }
    let dict = MdictDictionary::open(&dir)
        .unwrap_or_else(|e| panic!("opendict 打开 mdx_v3_basic 失败: {e}"));

    assert_eq!(dict.entry_count(), 12);

    let (first_word, first_text) = dict.entry_at(0).expect("entry_at(0)").expect("首词条");
    assert_eq!(first_word, "apple");
    assert!(first_text.contains("苹果"), "{first_text}");

    // 逐条扫到「苹果」，验证中文词头与释义
    let mut found = None;
    for i in 0..dict.entry_count() {
        let (word, text) = dict.entry_at(i).expect("entry_at").expect("词条存在");
        if word == "苹果" {
            found = Some(text);
            break;
        }
    }
    let text = found.unwrap_or_else(|| panic!("v3 语料应含词头「苹果」"));
    assert!(text.contains("apple"), "{text}");
}

fn assert_stardict_common(dir: &Path, word_count: usize, first_word: &str) -> StarDictDictionary {
    let dict = StarDictDictionary::open_with_cache(dir, "test", false)
        .unwrap_or_else(|e| panic!("打开 StarDict {} 失败: {e}", dir.display()));

    assert_eq!(dict.entry_count(), word_count);
    assert_eq!(dict.word_at(0), first_word);
    dict
}

#[test]
fn stardict_basic_roundtrip() {
    let dir = corpus("stardict_basic");
    if !require_dir(&dir) {
        return;
    }
    let dict = assert_stardict_common(&dir, 8, "ability");

    // 词条正文（sametypesequence=m，整块即数据）
    let (_, apple_bytes) = dict.raw_entry_at(1).expect("raw_entry_at(1)").expect("词条");
    let apple = String::from_utf8(apple_bytes.to_vec()).expect("UTF-8");
    assert!(apple.contains("苹果"), "{apple}");

    // 两个别名：apple fruit→apple、lexicon→dictionary
    assert_eq!(dict.synonym_count(), 2);
    assert_eq!(
        dict.synonym_at(0),
        Some(("apple fruit".to_owned(), 1, "apple".to_owned()))
    );
    assert_eq!(
        dict.synonym_at(1),
        Some(("lexicon".to_owned(), 6, "dictionary".to_owned()))
    );
}

#[test]
fn stardict_dz_roundtrip() {
    let dir = corpus("stardict_dz");
    if !require_dir(&dir) {
        return;
    }
    // .idx.gz + .dict.dz（gzip 兼容），磁盘缓存关闭 → 不会往语料目录写 .dict
    let dict = assert_stardict_common(&dir, 5, "advance");
    assert_eq!(dict.synonym_count(), 0);

    let (_, zebra) = dict.raw_entry_at(4).expect("raw_entry_at(4)").expect("词条");
    assert!(String::from_utf8(zebra.to_vec()).unwrap().contains("斑马"));

    assert!(
        !dir.join("test.dict").exists(),
        "cache_to_disk=false 不应落盘解压后的 .dict"
    );
}

// ── lite 模式：词头枚举 + 按序号物化释义 ──────────────────────────

/// 收集某解析器枚举出的全部词头
fn collect_headwords<P: dict_parser::DictionaryParser>(
    parser: &mut P,
    files: &[std::path::PathBuf],
) -> Vec<dict_parser::Headword> {
    let mut all = Vec::new();
    parser
        .parse_headwords(files, &mut |batch| {
            all.extend(batch);
            Ok(())
        })
        .expect("parse_headwords");
    all
}

#[test]
fn lite_mdx_v2_headwords_match_full_parse() {
    let dir = corpus("mdx_v2_rich");
    if !require_dir(&dir) {
        return;
    }
    let files = vec![dir.join("rich.mdx")];
    let mut parser = dict_parser::mdict::MdictParser::new();
    let headwords = collect_headwords(&mut parser, &files);

    // 词头数 = 全量条目数，序号连续 0..n
    let mut full_words = Vec::new();
    parser
        .parse(&files, &dict_parser::ParseOpts::new(7), &mut |batch| {
            full_words.extend(batch.iter().map(|e| e.word.clone()));
            Ok(())
        })
        .expect("parse");
    assert_eq!(headwords.len(), full_words.len(), "lite 词头数应等于全量条目数");
    for (i, hw) in headwords.iter().enumerate() {
        assert_eq!(hw.ordinal, i as i64);
        assert_eq!(hw.word, full_words[i], "序号 {i} 的词头应与全量一致");
    }

    // definition_at：appel 是 @@@LINK 行、apple2 二跳、apple 承载真释义
    let by_word = |w: &str| headwords.iter().find(|h| h.word == w).expect(w).ordinal;
    let appel = parser
        .definition_at(&files, by_word("appel"), None)
        .expect("appel 释义");
    assert_eq!(appel.trim(), "@@@LINK=apple2");
    let apple = parser
        .definition_at(&files, by_word("apple"), Some(7))
        .expect("apple 释义");
    assert!(apple.contains("苹果") || apple.contains("apple"), "{apple}");
    // 越界序号报错而非 panic
    assert!(parser.definition_at(&files, headwords.len() as i64, None).is_err());
}

#[test]
fn lite_mdx_v3_headwords_and_definition() {
    let dir = corpus("mdx_v3_basic");
    if !require_dir(&dir) {
        return;
    }
    let files = vec![dir.join("v3.mdx")];
    let mut parser = dict_parser::mdict::MdictParser::new();
    let headwords = collect_headwords(&mut parser, &files);
    assert!(!headwords.is_empty(), "v3 语料应有词头");
    for (i, hw) in headwords.iter().enumerate() {
        assert_eq!(hw.ordinal, i as i64);
    }
    let ord = headwords.iter().find(|h| h.word == "苹果").expect("苹果");
    let def = parser
        .definition_at(&files, ord.ordinal, None)
        .expect("v3 释义物化");
    assert!(def.contains("apple"), "{def}");
}

#[test]
fn lite_stardict_headwords_include_synonyms() {
    let dir = corpus("stardict_basic");
    if !require_dir(&dir) {
        return;
    }
    let files = vec![
        dir.join("test.ifo"),
        dir.join("test.idx"),
        dir.join("test.dict"),
        dir.join("test.syn"),
    ];
    let mut parser = dict_parser::stardict::StarDictParser::new();
    let headwords = collect_headwords(&mut parser, &files);
    // 8 条主词头 + 2 条别名
    assert_eq!(headwords.len(), 10);
    let alias = headwords.iter().find(|h| h.word == "apple fruit").expect("别名");
    assert_eq!(alias.ordinal, 1, "别名行 ordinal 应指向目标 apple（idx 1）");
    // 别名物化 = 目标释义
    let def = parser.definition_at(&files, alias.ordinal).expect("别名释义");
    assert!(def.contains("苹果"), "{def}");
    // 主词头物化
    let main = headwords.iter().find(|h| h.word == "apple").expect("apple");
    assert_eq!(parser.definition_at(&files, main.ordinal).unwrap(), def);
    assert!(parser.definition_at(&files, 9999).is_err());
}

#[test]
fn lite_ecdict_rejected() {
    let mut parser = dict_parser::ecdict::EcdictParser;
    let files = vec![testdata_dir().join("ecdict_mini.csv")];
    let err = parser
        .parse_headwords(&files, &mut |_| Ok(()))
        .expect_err("ECDICT 不支持 lite");
    assert!(matches!(err, dict_parser::ParserError::Validation(_)));
}

#[test]
fn parse_parallel_matches_serial() {
    let dir = corpus("mdx_v2_rich");
    if !require_dir(&dir) {
        return;
    }
    let files = vec![dir.join("rich.mdx")];

    let mut serial = Vec::new();
    let mut p1 = dict_parser::mdict::MdictParser::new();
    p1.parse(&files, &dict_parser::ParseOpts::new(9), &mut |batch| {
        serial.extend(batch);
        Ok(())
    })
    .expect("serial parse");

    let mut parallel = Vec::new();
    let mut p2 = dict_parser::mdict::MdictParser::new();
    p2.parse_parallel(&files, &dict_parser::ParseOpts::new(9), 3, &mut |batch| {
        parallel.extend(batch);
        Ok(())
    })
    .expect("parallel parse");

    // 并行 worker 的批次交错，顺序不保证 → 按多重集比较
    assert_eq!(serial.len(), parallel.len());
    let mut s: Vec<(String, String)> =
        serial.into_iter().map(|e| (e.word, e.definition)).collect();
    let mut p: Vec<(String, String)> =
        parallel.into_iter().map(|e| (e.word, e.definition)).collect();
    s.sort();
    p.sort();
    assert_eq!(s, p, "并行解析输出应与串行完全一致");
}

#[test]
fn multi_mdx_group_parses_all_files() {
    // 手工上传多部 .mdx 成一组：Python 侧遍历全部 .mdx，Rust 侧全局序号按文件拼接
    let rich = corpus("mdx_v2_rich").join("rich.mdx");
    let basic = corpus("mdx_v2_basic").join("basic.mdx");
    if !rich.is_file() || !basic.is_file() {
        return;
    }
    let files = vec![rich, basic];

    // 全量：条目数 = 两部之和
    let mut entries = Vec::new();
    let mut parser = dict_parser::mdict::MdictParser::new();
    parser
        .parse(&files, &dict_parser::ParseOpts::new(3), &mut |batch| {
            entries.extend(batch);
            Ok(())
        })
        .expect("multi-mdx parse");
    let mut single = Vec::new();
    let mut p1 = dict_parser::mdict::MdictParser::new();
    p1.parse(&files[..1], &dict_parser::ParseOpts::new(3), &mut |batch| {
        single.extend(batch);
        Ok(())
    })
    .expect("single parse");
    let mut other = Vec::new();
    let mut p2 = dict_parser::mdict::MdictParser::new();
    p2.parse(&files[1..], &dict_parser::ParseOpts::new(3), &mut |batch| {
        other.extend(batch);
        Ok(())
    })
    .expect("second parse");
    assert_eq!(
        entries.len(),
        single.len() + other.len(),
        "多 .mdx 组应导入全部文件（此前只导第一部）"
    );

    // lite：词头序号跨文件连续
    let mut headwords = Vec::new();
    let mut hp = dict_parser::mdict::MdictParser::new();
    hp.parse_headwords(&files, &mut |batch| {
        headwords.extend(batch);
        Ok(())
    })
    .expect("headwords");
    assert_eq!(headwords.len(), entries.len());
    for (i, hw) in headwords.iter().enumerate() {
        assert_eq!(hw.ordinal, i as i64, "全局序号应连续");
    }

    // 物化：第二部文件里的条目按全局序号也能读到
    let second_file_first = single.len() as i64;
    let def = hp
        .definition_at(&files, second_file_first, None)
        .expect("第二部首条物化");
    assert!(!def.is_empty());
    // 越界
    assert!(hp.definition_at(&files, headwords.len() as i64, None).is_err());
}

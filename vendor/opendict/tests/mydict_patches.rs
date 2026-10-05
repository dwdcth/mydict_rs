//! Regression tests for the MYDICT-PATCH additions to this fork:
//! ① MDD/词头枚举与位置访问  ② StarDict cache_to_disk 开关
//! ③ header StyleSheet/Compact 暴露  ④ Dict::read_raw  ⑤ 位置访问

use opendict::mdict::MdictDictionary;
use opendict::stardict::StarDictDictionary;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ── Patch ①/③: MDict 位置访问 + header ──────────────────────────

#[test]
fn mdict_entry_at_returns_all_entries_in_order() {
    let dict = MdictDictionary::open(&fixture_dir()).unwrap();
    assert_eq!(dict.entry_count(), 3);

    let (w0, t0) = dict.entry_at(0).unwrap().unwrap();
    assert_eq!((w0.as_str(), t0.as_str()), ("foo", "bar"));
    let (w1, t1) = dict.entry_at(1).unwrap().unwrap();
    assert_eq!((w1.as_str(), t1.as_str()), ("hello", "<b>hello</b> greeting"));
    let (w2, t2) = dict.entry_at(2).unwrap().unwrap();
    assert_eq!((w2.as_str(), t2.as_str()), ("test", "test data here"));

    assert!(dict.entry_at(3).unwrap().is_none());
}

#[test]
fn mdict_header_exposes_engine_version_and_optional_fields() {
    let dict = MdictDictionary::open(&fixture_dir()).unwrap();
    let header = dict.header();
    assert_eq!(header.engine_version, "2.0");
    assert_eq!(header.style_sheet, None);
    assert!(!header.compact);
}

#[test]
fn mdict_mdd_resource_keys_empty_without_mdd() {
    let dict = MdictDictionary::open(&fixture_dir()).unwrap();
    assert!(dict.mdd_resource_keys().is_empty());
}

// ── Patch ②: StarDict cache_to_disk 开关 ────────────────────────

#[test]
fn stardict_open_without_cache_writes_nothing() {
    let dir = tempdir();
    // 只拷贝 .ifo/.idx 和 .dict.dz —— 不拷 .dict，验证 nocache 打开不落盘
    for name in ["testdict.ifo", "testdict.idx", "testdict.dict.dz", "testdict.syn"] {
        std::fs::copy(fixture_dir().join(name), dir.path().join(name)).unwrap();
    }

    let dict = StarDictDictionary::open_dir_with_cache(dir.path(), false).unwrap();
    assert_eq!(dict.entry_count(), 4);
    assert!(dict.lookup("foo").unwrap().is_some());
    assert!(
        !dir.path().join("testdict.dict").exists(),
        "nocache 打开不得在词典目录写解压缓存"
    );
}

#[test]
fn stardict_open_with_cache_writes_decompressed_dict() {
    let dir = tempdir();
    for name in ["testdict.ifo", "testdict.idx", "testdict.dict.dz", "testdict.syn"] {
        std::fs::copy(fixture_dir().join(name), dir.path().join(name)).unwrap();
    }

    let _dict = StarDictDictionary::open_dir_with_cache(dir.path(), true).unwrap();
    assert!(
        dir.path().join("testdict.dict").exists(),
        "cache 打开应落解压缓存（上游默认行为）"
    );
}

// ── Patch ④/⑤: StarDict 位置访问 + read_raw + 同义词枚举 ────────

#[test]
fn stardict_positional_access() {
    // fixtures 目录里有 multitype.* 和 testdict.* 两部词典，open_dir 取哪个
    // .ifo 取决于目录序 —— 用显式名字打开。
    let dict = StarDictDictionary::open_with_cache(&fixture_dir(), "testdict", false).unwrap();
    assert_eq!(dict.entry_count(), 4);
    assert_eq!(dict.word_at(0), "another");
    assert_eq!(dict.word_at(3), "some word");

    let (word, raw) = dict.raw_entry_at(1).unwrap().unwrap();
    assert_eq!(word, "foo");
    assert_eq!(raw, b"bar"); // .dict 偏移 8 处的 3 字节（见 testdict.idx 上游测试）
}

#[test]
fn stardict_synonym_enumeration() {
    let dict = StarDictDictionary::open_with_cache(&fixture_dir(), "testdict", false).unwrap();
    assert_eq!(dict.synonym_count(), 2);
    let (alias, index, target) = dict.synonym_at(0).unwrap();
    assert_eq!(alias, "abc");
    assert_eq!(index, 3);
    assert_eq!(target, "some word");
    let (alias, _, target) = dict.synonym_at(1).unwrap();
    assert_eq!(alias, "synonym two");
    assert_eq!(target, "lorem");
    assert!(dict.synonym_at(2).is_none());
}

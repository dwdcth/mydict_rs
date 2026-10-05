# Test Reorganisation

## Summary

Move unit tests out of `tests/` into `#[cfg(test)]` modules inside the source files they test. Remove `extern crate` from remaining integration tests. Delete debug example scripts.

## Rationale

- `tests/` is for integration tests that exercise the public API. Five of the eight test files (`dict.rs`, `idx.rs`, `ifo.rs`, `syn.rs`, `strcmp.rs`) test internal module details through leaked re-exports in `lib.rs`. When those re-exports are removed (see error-handling-plan), these tests break.
- Moving them into the source modules makes them proper unit tests with direct access to the types they test.
- `extern crate opendict;` is unnecessary since Rust 2018 edition.

## Files to delete

```
tests/dict.rs
tests/idx.rs
tests/ifo.rs
tests/syn.rs
tests/strcmp.rs
```

## Files to keep (integration tests)

```
tests/integration.rs      — StarDict full lifecycle through public API
tests/mdict_fixture.rs    — MDict fixture parsing through public API
tests/real_dicts.rs        — Real dictionary smoke tests (ignored by default)
```

## Changes to remaining integration tests

### tests/integration.rs

Remove line 5:

```diff
-extern crate opendict;
```

### tests/real_dicts.rs

Remove line 10:

```diff
-extern crate opendict;
```

### tests/mdict_fixture.rs

No changes needed (already has no `extern crate`).

---

## Tests to move into source modules

### tests/strcmp.rs → src/stardict/strcmp.rs

Append to end of `src/stardict/strcmp.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::stardict_strcmp;

    #[test]
    fn equal_strings() {
        assert_eq!(stardict_strcmp("apple", "apple"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn shorter_string_is_less() {
        assert_eq!(
            stardict_strcmp("apple", "appleee"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn case_insensitive_then_length() {
        assert_eq!(
            stardict_strcmp("apple", "ApPleee"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn case_sensitive_tiebreaker() {
        assert_eq!(
            stardict_strcmp("apple", "ApPle"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn different_words_case_insensitive() {
        assert_eq!(
            stardict_strcmp("apple", "pEar"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn different_words_same_start() {
        assert_eq!(
            stardict_strcmp("pear", "pineapple"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn non_ascii_in_second_word() {
        assert_eq!(
            stardict_strcmp("pear", "pineäpple"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn non_ascii_not_folded() {
        assert_eq!(
            stardict_strcmp("pear", "peär"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn non_ascii_not_folded_longer() {
        assert_eq!(
            stardict_strcmp("pear", "peärs"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn non_ascii_length_difference() {
        assert_eq!(
            stardict_strcmp("peärs", "peär"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn hyphen_before_letter() {
        assert_eq!(
            stardict_strcmp("ap-ple", "apple"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn empty_strings_equal() {
        assert_eq!(stardict_strcmp("", ""), std::cmp::Ordering::Equal);
    }

    #[test]
    fn empty_vs_nonempty() {
        assert_eq!(stardict_strcmp("", "a"), std::cmp::Ordering::Less);
        assert_eq!(stardict_strcmp("a", ""), std::cmp::Ordering::Greater);
    }

    #[test]
    fn single_char_case_tiebreaker() {
        assert_eq!(
            stardict_strcmp("a", "A"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn case_insensitive_ordering_ignores_case() {
        assert_eq!(
            stardict_strcmp("B", "a"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn digits_not_folded() {
        assert_eq!(
            stardict_strcmp("a1", "a2"),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            stardict_strcmp("a2", "a1"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn symmetric() {
        let a = "apple";
        let b = "banana";
        let cmp1 = stardict_strcmp(a, b);
        let cmp2 = stardict_strcmp(b, a);
        assert_eq!(cmp1, std::cmp::Ordering::Less);
        assert_eq!(cmp2, std::cmp::Ordering::Greater);
    }

    #[test]
    fn transitive() {
        assert_eq!(stardict_strcmp("ant", "bat"), std::cmp::Ordering::Less);
        assert_eq!(stardict_strcmp("bat", "cat"), std::cmp::Ordering::Less);
        assert_eq!(stardict_strcmp("ant", "cat"), std::cmp::Ordering::Less);
    }
}
```

---

### tests/ifo.rs → src/stardict/ifo.rs

Append to end of `src/stardict/ifo.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    // ── Magic line validation ────────────────────────────────────────────

    #[test]
    fn rejects_wrong_magic_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ifo");
        std::fs::write(
            &path,
            "Wrong magic line\nversion=3.0.0\nbookname=Test\nwordcount=1\nidxfilesize=10\n",
        )
        .unwrap();
        let result = Ifo::open(path);
        assert!(result.is_err(), "Should reject wrong magic line");
    }

    #[test]
    fn accepts_correct_magic_line() {
        let result = Ifo::open(fixture("testdict.ifo"));
        assert!(result.is_ok(), "Should accept correct magic line");
    }

    // ── Version parsing ──────────────────────────────────────────────────

    #[test]
    fn parses_v300() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.version, "3.0.0");
    }

    #[test]
    fn parses_v242() {
        let ifo = Ifo::open(fixture("v242.ifo")).unwrap();
        assert_eq!(ifo.version, "2.4.2");
    }

    #[test]
    fn rejects_unknown_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_ver.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=1.0.0\nbookname=Test\nwordcount=1\nidxfilesize=10\n",
        )
        .unwrap();
        let result = Ifo::open(path);
        assert!(result.is_err(), "Should reject unknown version");
    }

    // ── Field parsing ────────────────────────────────────────────────────

    #[test]
    fn parses_bookname() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.name, "A foo-bar dictionary");
    }

    #[test]
    fn parses_wordcount() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.word_count, 4);
    }

    #[test]
    fn parses_idxfilesize() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.idx_file_size, 60);
    }

    #[test]
    fn parses_sametypesequence() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.same_type_sequence, "m");
    }

    #[test]
    fn parses_description_empty() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.description, "");
    }

    #[test]
    fn parses_v242_bookname() {
        let ifo = Ifo::open(fixture("v242.ifo")).unwrap();
        assert_eq!(ifo.name, "Test Dict v242");
    }

    #[test]
    fn parses_v242_wordcount() {
        let ifo = Ifo::open(fixture("v242.ifo")).unwrap();
        assert_eq!(ifo.word_count, 2);
    }

    #[test]
    fn parses_v242_idxfilesize() {
        let ifo = Ifo::open(fixture("v242.ifo")).unwrap();
        assert_eq!(ifo.idx_file_size, 24);
    }

    // ── Optional fields ──────────────────────────────────────────────────

    #[test]
    fn optional_author_defaults_empty() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.author, "");
    }

    #[test]
    fn optional_email_defaults_empty() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.email, "");
    }

    #[test]
    fn optional_website_defaults_empty() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.web_site, "");
    }

    #[test]
    fn idxoffsetbits_defaults_to_32() {
        let ifo = Ifo::open(fixture("testdict.ifo")).unwrap();
        assert_eq!(ifo.idx_offset_bits, 32);
    }

    #[test]
    fn parses_idxoffsetbits_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bits64.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nbookname=Test\nwordcount=1\nidxfilesize=10\nidxoffsetbits=64\n",
        )
        .unwrap();
        let ifo = Ifo::open(path).unwrap();
        assert_eq!(ifo.idx_offset_bits, 64);
    }

    #[test]
    fn synwordcount_defaults_to_zero() {
        let ifo = Ifo::open(fixture("v242.ifo")).unwrap();
        assert_eq!(ifo.syn_word_count, 0);
    }

    // ── Whitespace handling ──────────────────────────────────────────────

    #[test]
    fn trims_whitespace_around_equals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spaces.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nbookname = Spaced Name \nwordcount = 4 \nidxfilesize = 60 \n",
        )
        .unwrap();
        let ifo = Ifo::open(path).unwrap();
        assert_eq!(ifo.name, "Spaced Name");
        assert_eq!(ifo.word_count, 4);
        assert_eq!(ifo.idx_file_size, 60);
    }

    #[test]
    fn blank_lines_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blanks.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\n\nversion=3.0.0\n\nbookname=Blanky\n\nwordcount=1\nidxfilesize=10\n\n",
        )
        .unwrap();
        let ifo = Ifo::open(path).unwrap();
        assert_eq!(ifo.name, "Blanky");
    }

    // ── Required fields ──────────────────────────────────────────────────

    #[test]
    fn missing_bookname_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_name.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nwordcount=1\nidxfilesize=10\n",
        )
        .unwrap();
        let result = Ifo::open(path);
        assert!(result.is_err(), "Should error on missing bookname");
    }

    #[test]
    fn missing_wordcount_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_wc.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nbookname=Test\nidxfilesize=10\n",
        )
        .unwrap();
        let result = Ifo::open(path);
        assert!(result.is_err(), "Should error on missing wordcount");
    }

    #[test]
    fn missing_idxfilesize_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_idx.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nbookname=Test\nwordcount=1\n",
        )
        .unwrap();
        let result = Ifo::open(path);
        assert!(result.is_err(), "Should error on missing idxfilesize");
    }

    // ── Numeric type correctness ─────────────────────────────────────────

    #[test]
    fn wordcount_is_u32_not_isize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\nversion=3.0.0\nbookname=Big\nwordcount=3000000000\nidxfilesize=99999999\n",
        )
        .unwrap();
        let ifo = Ifo::open(path).unwrap();
        assert_eq!(ifo.word_count, 3_000_000_000);
    }

    // ── All optional fields when populated ───────────────────────────────

    #[test]
    fn parses_all_optional_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.ifo");
        std::fs::write(
            &path,
            "StarDict's dict ifo file\n\
             version=3.0.0\n\
             bookname=Full Dict\n\
             wordcount=100\n\
             idxfilesize=5000\n\
             author=Jane Doe\n\
             email=jane@example.com\n\
             website=https://example.com\n\
             description=A test dictionary\n\
             date=2024.01.01\n\
             sametypesequence=m\n\
             synwordcount=10\n\
             idxoffsetbits=64\n",
        )
        .unwrap();
        let ifo = Ifo::open(path).unwrap();
        assert_eq!(ifo.author, "Jane Doe");
        assert_eq!(ifo.email, "jane@example.com");
        assert_eq!(ifo.web_site, "https://example.com");
        assert_eq!(ifo.description, "A test dictionary");
        assert_eq!(ifo.date, "2024.01.01");
        assert_eq!(ifo.same_type_sequence, "m");
        assert_eq!(ifo.syn_word_count, 10);
        assert_eq!(ifo.idx_offset_bits, 64);
    }
}
```

---

### tests/idx.rs → src/stardict/idx.rs

Append to end of `src/stardict/idx.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn build_idx_32(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (word, offset, size) in entries {
            buf.extend_from_slice(word.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&offset.to_be_bytes());
            buf.extend_from_slice(&size.to_be_bytes());
        }
        buf
    }

    fn build_idx_64(entries: &[(&str, u64, u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (word, offset, size) in entries {
            buf.extend_from_slice(word.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&offset.to_be_bytes());
            buf.extend_from_slice(&size.to_be_bytes());
        }
        buf
    }

    #[test]
    fn parses_all_four_entries() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        assert_eq!(idx.entry_count(), 4);

        let e = idx.entry(0);
        assert_eq!(e.word, "another");
        assert_eq!(e.offset, 0);
        assert_eq!(e.size, 8);

        let e = idx.entry(1);
        assert_eq!(e.word, "foo");
        assert_eq!(e.offset, 8);
        assert_eq!(e.size, 3);

        let e = idx.entry(2);
        assert_eq!(e.word, "lorem");
        assert_eq!(e.offset, 11);
        assert_eq!(e.size, 5);

        let e = idx.entry(3);
        assert_eq!(e.word, "some word");
        assert_eq!(e.offset, 16);
        assert_eq!(e.size, 13);
    }

    #[test]
    fn entry_count_matches_wordcount() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        assert_eq!(idx.entry_count(), 4);
    }

    #[test]
    fn total_bytes_matches_idxfilesize() {
        let data = std::fs::read(fixture("testdict.idx")).unwrap();
        assert_eq!(data.len(), 60);
    }

    #[test]
    fn entries_are_sorted_by_stardict_strcmp() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        for i in 0..idx.entry_count() - 1 {
            assert!(
                idx.word_at(i) < idx.word_at(i + 1)
                    || idx.word_at(i) == idx.word_at(i + 1),
                "Entries should be sorted: {:?} should come before {:?}",
                idx.word_at(i),
                idx.word_at(i + 1)
            );
        }
    }

    #[test]
    fn words_are_valid_utf8() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        for i in 0..idx.entry_count() {
            assert!(!idx.word_at(i).is_empty());
        }
    }

    #[test]
    fn binary_search_finds_existing_word() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        let result = idx.search("foo");
        assert!(result.is_some(), "Should find 'foo'");
        let entry = result.unwrap();
        assert_eq!(entry.word, "foo");
        assert_eq!(entry.offset, 8);
        assert_eq!(entry.size, 3);
    }

    #[test]
    fn binary_search_finds_first_word() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        let result = idx.search("another");
        assert!(result.is_some());
        assert_eq!(result.unwrap().word, "another");
    }

    #[test]
    fn binary_search_finds_last_word() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        let result = idx.search("some word");
        assert!(result.is_some());
        assert_eq!(result.unwrap().word, "some word");
    }

    #[test]
    fn binary_search_returns_none_for_missing_word() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        let result = idx.search("nonexistent");
        assert!(result.is_none(), "Should return None for missing word");
    }

    #[test]
    fn binary_search_returns_none_for_empty_string() {
        let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
        let result = idx.search("");
        assert!(result.is_none());
    }

    #[test]
    fn parses_64bit_offsets() {
        let data = build_idx_64(&[
            ("alpha", 0, 10),
            ("beta", 0x1_0000_0000, 20),
        ]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test64.idx");
        std::fs::write(&path, &data).unwrap();

        let idx = Idx::open(path, 2, 64).unwrap();
        assert_eq!(idx.entry_count(), 2);

        let e = idx.entry(0);
        assert_eq!(e.word, "alpha");
        assert_eq!(e.offset, 0);
        assert_eq!(e.size, 10);

        let e = idx.entry(1);
        assert_eq!(e.word, "beta");
        assert_eq!(e.offset, 0x1_0000_0000);
        assert_eq!(e.size, 20);
    }

    #[test]
    fn parses_synthetic_32bit_idx() {
        let data = build_idx_32(&[("cat", 0, 5), ("dog", 5, 7)]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synth.idx");
        std::fs::write(&path, &data).unwrap();

        let idx = Idx::open(path, 2, 32).unwrap();
        assert_eq!(idx.entry_count(), 2);

        let e = idx.entry(0);
        assert_eq!(e.word, "cat");
        assert_eq!(e.offset, 0);
        assert_eq!(e.size, 5);

        let e = idx.entry(1);
        assert_eq!(e.word, "dog");
        assert_eq!(e.offset, 5);
        assert_eq!(e.size, 7);
    }
}
```

---

### tests/syn.rs → src/stardict/syn.rs

Append to end of `src/stardict/syn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn build_syn(entries: &[(&str, u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (word, index) in entries {
            buf.extend_from_slice(word.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&index.to_be_bytes());
        }
        buf
    }

    #[test]
    fn parses_two_entries() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();
        assert_eq!(syn.entry_count(), 2);
    }

    #[test]
    fn first_entry_abc_points_to_index_3() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();
        let e = syn.entry(0);
        assert_eq!(e.word, "abc");
        assert_eq!(e.original_word_index, 3);
    }

    #[test]
    fn second_entry_synonym_two_points_to_index_2() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();
        let e = syn.entry(1);
        assert_eq!(e.word, "synonym two");
        assert_eq!(e.original_word_index, 2);
    }

    #[test]
    fn synwordcount_matches_entry_count() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();
        assert_eq!(syn.entry_count(), 2);
    }

    #[test]
    fn synonym_word_is_null_terminated_utf8() {
        let data = std::fs::read(fixture("testdict.syn")).unwrap();
        assert_eq!(&data[0..3], b"abc");
        assert_eq!(data[3], 0);
        assert_eq!(&data[8..19], b"synonym two");
        assert_eq!(data[19], 0);
    }

    #[test]
    fn original_word_index_is_u32_big_endian() {
        let data = std::fs::read(fixture("testdict.syn")).unwrap();
        assert_eq!(&data[4..8], &[0, 0, 0, 3]);
        assert_eq!(&data[20..24], &[0, 0, 0, 2]);
    }

    #[test]
    fn entries_are_sorted() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();
        for i in 0..syn.entry_count() - 1 {
            assert!(
                syn.word_at(i) <= syn.word_at(i + 1),
                "SYN entries should be sorted: {:?} should come before {:?}",
                syn.word_at(i),
                syn.word_at(i + 1)
            );
        }
    }

    #[test]
    fn parses_synthetic_syn_file() {
        let data = build_syn(&[("alpha", 0), ("bravo", 1), ("charlie", 2)]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synth.syn");
        std::fs::write(&path, &data).unwrap();

        let syn = Syn::open(path, 3).unwrap();
        assert_eq!(syn.entry_count(), 3);

        let e = syn.entry(0);
        assert_eq!(e.word, "alpha");
        assert_eq!(e.original_word_index, 0);

        let e = syn.entry(1);
        assert_eq!(e.word, "bravo");
        assert_eq!(e.original_word_index, 1);

        let e = syn.entry(2);
        assert_eq!(e.word, "charlie");
        assert_eq!(e.original_word_index, 2);
    }

    #[test]
    fn lookup_synonym_by_word() {
        let syn = Syn::open(fixture("testdict.syn"), 2).unwrap();

        let result = syn.lookup("abc");
        assert!(result.is_some(), "Should find synonym 'abc'");
        assert_eq!(result.unwrap().original_word_index, 3);

        let result = syn.lookup("synonym two");
        assert!(result.is_some());
        assert_eq!(result.unwrap().original_word_index, 2);

        let result = syn.lookup("nonexistent");
        assert!(result.is_none());
    }
}
```

---

### tests/dict.rs → src/stardict/dict.rs

Append to end of `src/stardict/dict.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    // ── sametypesequence=m (testdict.dict, 29 bytes) ─────────────────────

    #[test]
    fn reads_another_at_offset_0() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let entries = dict.read_entry(0, 8, Some("m"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"translat");
    }

    #[test]
    fn reads_foo_at_offset_8() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let entries = dict.read_entry(8, 3, Some("m"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"bar");
    }

    #[test]
    fn reads_lorem_at_offset_11() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let entries = dict.read_entry(11, 5, Some("m"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"ipsum");
    }

    #[test]
    fn reads_some_word_at_offset_16() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let entries = dict.read_entry(16, 13, Some("m"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"a translation");
    }

    #[test]
    fn last_entry_has_no_null_terminator() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let entries = dict.read_entry(16, 13, Some("m"));
        assert_eq!(entries[0].data, b"a translation");
        assert_ne!(entries[0].data.last(), Some(&0u8));
    }

    #[test]
    fn all_entries_type_m() {
        let dict = Dict::open(fixture("testdict.dict"), false).unwrap();
        let offsets: &[(u64, u32)] = &[(0, 8), (8, 3), (11, 5), (16, 13)];
        let expected_data: &[&[u8]] = &[b"translat", b"bar", b"ipsum", b"a translation"];
        for (i, &(offset, size)) in offsets.iter().enumerate() {
            let entries = dict.read_entry(offset, size, Some("m"));
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].type_id, 'm');
            assert_eq!(entries[0].data, expected_data[i]);
        }
    }

    // ── sametypesequence=tm (synthetic) ──────────────────────────────────

    #[test]
    fn sametypesequence_tm_two_fields() {
        let phonetic = b"helo\0";
        let meaning = b"greeting word";
        let mut data = Vec::new();
        data.extend_from_slice(phonetic);
        data.extend_from_slice(meaning);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tm.dict");
        std::fs::write(&path, &data).unwrap();

        let dict = Dict::open(path, false).unwrap();
        let entries = dict.read_entry(0, data.len() as u32, Some("tm"));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].type_id, 't');
        assert_eq!(entries[0].data, b"helo");
        assert_eq!(entries[1].type_id, 'm');
        assert_eq!(entries[1].data, b"greeting word");
    }

    // ── No sametypesequence (multitype.dict) ─────────────────────────────

    #[test]
    fn no_sametypesequence_entry1_two_fields() {
        let dict = Dict::open(fixture("multitype.dict"), false).unwrap();
        let entries = dict.read_entry(0, 20, None);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].type_id, 'h');
        assert_eq!(entries[0].data, b"<b>hello</b>");
        assert_eq!(entries[1].type_id, 't');
        assert_eq!(entries[1].data, b"helo");
    }

    #[test]
    fn no_sametypesequence_entry2_one_field() {
        let dict = Dict::open(fixture("multitype.dict"), false).unwrap();
        let entries = dict.read_entry(20, 11, None);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"the world");
    }

    // ── Uppercase type identifiers (binary data with u32 length) ─────────

    #[test]
    fn uppercase_type_has_u32_length_prefix() {
        let wav_data = b"\x00\x01\x02\x03\x04";
        let mut data = Vec::new();
        data.push(b'W');
        data.extend_from_slice(&(wav_data.len() as u32).to_be_bytes());
        data.extend_from_slice(wav_data);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upper.dict");
        std::fs::write(&path, &data).unwrap();

        let dict = Dict::open(path, false).unwrap();
        let entries = dict.read_entry(0, data.len() as u32, None);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].type_id, 'W');
        assert_eq!(entries[0].data, wav_data);
    }

    #[test]
    fn mixed_lowercase_and_uppercase_types() {
        let mut data = Vec::new();
        data.push(b'm');
        data.extend_from_slice(b"meaning\0");
        data.push(b'P');
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mixed.dict");
        std::fs::write(&path, &data).unwrap();

        let dict = Dict::open(path, false).unwrap();
        let entries = dict.read_entry(0, data.len() as u32, None);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].type_id, 'm');
        assert_eq!(entries[0].data, b"meaning");
        assert_eq!(entries[1].type_id, 'P');
        assert_eq!(entries[1].data, &[0xAA, 0xBB, 0xCC]);
    }
}
```

---

## Examples to delete

These are debug scripts with hardcoded paths, not useful as library examples:

```
examples/debug_missing.rs
examples/debug_search.rs
examples/debug_sort.rs
```

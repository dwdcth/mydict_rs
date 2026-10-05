# Memory Optimization Plan

## Problem

Currently each dictionary entry is parsed into individual Rust structs with
separate heap-allocated Strings. For Langdao (405k entries), the idx alone
uses 20.5 MB — 2.4x the raw file size. Across all 4 test dictionaries, total
resident memory is 112 MB.

## Strategy

Three changes:

1. **Idx: raw bytes + offset table** — keep the file bytes as a single `Vec<u8>`,
   build a compact `Vec<u32>` of entry start positions. Binary search reads words
   directly from the buffer. Eliminates 405k separate String allocations.

2. **Syn: raw bytes + offset table** — same approach. The Spanish dictionary's
   988k synonym entries drop from 40.8 MB to ~16 MB.

3. **Dict: mmap uncompressed files** — for `.dict` files, memory-map instead of
   reading into a `Vec<u8>`. The OS loads pages on demand. For `.dict.dz` files,
   continue decompressing into memory (dictzip random access is future work).

## Expected Results

| Dictionary | Before | After | Reduction |
|---|---|---|---|
| 现代汉语词典 (57k) | 8.1 MB | ~2.0 MB | ~75% |
| 朗道汉英字典 (405k) | 33.1 MB | ~14.5 MB | ~56% |
| Korean-English (49k) | 17.9 MB | ~16.2 MB | ~10% |
| Spanish-English (98k) | 52.9 MB | ~16.0 MB | ~70% |
| **Total** | **112 MB** | **~49 MB** | **~56%** |

Note: Korean and Langdao use `.dict.dz` so the dict data stays as decompressed
`Vec<u8>`. Dictionaries with uncompressed `.dict` files would see near-zero dict
memory via mmap.

## File Changes

### 1. `Cargo.toml` — add memmap2

```toml
[dependencies]
anyhow = "1"
flate2 = "1"
memmap2 = "0.9"
```

### 2. `src/idx.rs` — full replacement

```rust
use std::{path, str};

use anyhow::anyhow;

use super::strcmp::stardict_strcmp;

/// A single index entry (constructed on demand, not stored).
#[derive(Debug, Clone)]
pub struct IdxEntry {
    pub word: String,
    pub offset: u64,
    pub size: u32,
}

/// Compact index: raw file bytes + entry offset table.
pub struct Idx {
    data: Vec<u8>,
    offsets: Vec<u32>,
    offset_bits: u32,
}

impl Idx {
    pub fn open(file: path::PathBuf, _word_count: u32, offset_bits: u32) -> anyhow::Result<Idx> {
        let data = crate::io::read_file(&file)?;
        let ref_size: usize = if offset_bits == 64 { 12 } else { 8 };

        let mut offsets = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            offsets.push(pos as u32);
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| anyhow!("idx: missing null terminator at offset {}", pos))?;
            // Validate UTF-8
            str::from_utf8(&data[pos..pos + null_pos])?;
            let ref_start = pos + null_pos + 1;
            if ref_start + ref_size > data.len() {
                return Err(anyhow!("idx: unexpected EOF at offset {}", ref_start));
            }
            pos = ref_start + ref_size;
        }

        Ok(Idx { data, offsets, offset_bits })
    }

    pub fn entry_count(&self) -> usize {
        self.offsets.len()
    }

    /// Word for entry i — zero-copy from raw buffer.
    pub fn word_at(&self, i: usize) -> &str {
        let start = self.offsets[i] as usize;
        let null_pos = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap();
        // Safety: validated UTF-8 during open()
        unsafe { str::from_utf8_unchecked(&self.data[start..start + null_pos]) }
    }

    /// Construct a full IdxEntry for entry i (allocates a String).
    pub fn entry(&self, i: usize) -> IdxEntry {
        let start = self.offsets[i] as usize;
        let null_pos = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap();
        let word = unsafe {
            str::from_utf8_unchecked(&self.data[start..start + null_pos])
        }.to_string();
        let ref_start = start + null_pos + 1;

        let (offset, size) = if self.offset_bits == 64 {
            let offset = u64::from_be_bytes([
                self.data[ref_start],     self.data[ref_start + 1],
                self.data[ref_start + 2], self.data[ref_start + 3],
                self.data[ref_start + 4], self.data[ref_start + 5],
                self.data[ref_start + 6], self.data[ref_start + 7],
            ]);
            let size = u32::from_be_bytes([
                self.data[ref_start + 8],  self.data[ref_start + 9],
                self.data[ref_start + 10], self.data[ref_start + 11],
            ]);
            (offset, size)
        } else {
            let offset = u32::from_be_bytes([
                self.data[ref_start],     self.data[ref_start + 1],
                self.data[ref_start + 2], self.data[ref_start + 3],
            ]) as u64;
            let size = u32::from_be_bytes([
                self.data[ref_start + 4], self.data[ref_start + 5],
                self.data[ref_start + 6], self.data[ref_start + 7],
            ]);
            (offset, size)
        };

        IdxEntry { word, offset, size }
    }

    pub fn search(&self, word: &str) -> Option<IdxEntry> {
        self.binary_search(word, |w, target| stardict_strcmp(w, target))
            .or_else(|| {
                self.binary_search(word, |w, target| w.as_bytes().cmp(target.as_bytes()))
            })
    }

    fn binary_search<F>(&self, word: &str, cmp: F) -> Option<IdxEntry>
    where
        F: Fn(&str, &str) -> std::cmp::Ordering,
    {
        let mut low = 0usize;
        let mut high = self.offsets.len();
        while low < high {
            let mid = low + (high - low) / 2;
            match cmp(self.word_at(mid), word) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(self.entry(mid)),
            }
        }
        None
    }

    /// Raw data size in bytes (for memory reporting).
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Number of offset table entries (for memory reporting).
    pub fn offsets_len(&self) -> usize {
        self.offsets.len()
    }
}
```

### 3. `src/syn.rs` — full replacement

```rust
use std::{fs, path, str};

use anyhow::anyhow;

/// A single synonym entry (constructed on demand, not stored).
#[derive(Debug, Clone)]
pub struct SynEntry {
    pub word: String,
    pub original_word_index: u32,
}

/// Compact synonym index: raw file bytes + entry offset table.
pub struct Syn {
    data: Vec<u8>,
    offsets: Vec<u32>,
}

impl Syn {
    pub fn open(file: path::PathBuf, _syn_word_count: u32) -> anyhow::Result<Syn> {
        let data = fs::read(&file)?;
        let mut offsets = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            offsets.push(pos as u32);
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| anyhow!("syn: missing null terminator at offset {}", pos))?;
            str::from_utf8(&data[pos..pos + null_pos])?;
            let idx_start = pos + null_pos + 1;
            if idx_start + 4 > data.len() {
                return Err(anyhow!("syn: unexpected EOF reading word index"));
            }
            pos = idx_start + 4;
        }

        Ok(Syn { data, offsets })
    }

    pub fn entry_count(&self) -> usize {
        self.offsets.len()
    }

    pub fn word_at(&self, i: usize) -> &str {
        let start = self.offsets[i] as usize;
        let null_pos = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap();
        unsafe { str::from_utf8_unchecked(&self.data[start..start + null_pos]) }
    }

    pub fn entry(&self, i: usize) -> SynEntry {
        let start = self.offsets[i] as usize;
        let null_pos = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap();
        let word = unsafe {
            str::from_utf8_unchecked(&self.data[start..start + null_pos])
        }.to_string();
        let idx_start = start + null_pos + 1;
        let original_word_index = u32::from_be_bytes([
            self.data[idx_start],     self.data[idx_start + 1],
            self.data[idx_start + 2], self.data[idx_start + 3],
        ]);
        SynEntry { word, original_word_index }
    }

    pub fn lookup(&self, word: &str) -> Option<SynEntry> {
        (0..self.offsets.len())
            .find(|&i| self.word_at(i) == word)
            .map(|i| self.entry(i))
    }

    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    pub fn offsets_len(&self) -> usize {
        self.offsets.len()
    }
}
```

### 4. `src/dict.rs` — full replacement

```rust
use std::fs::{self, File};
use std::io::Read;
use std::path;

use flate2::read::GzDecoder;
use memmap2::Mmap;

#[derive(Debug, Clone)]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

enum DictData {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

pub struct Dict {
    data: DictData,
}

impl Dict {
    pub fn open(file: path::PathBuf) -> anyhow::Result<Dict> {
        let is_compressed = file.extension()
            .map_or(false, |ext| ext == "gz" || ext == "dz");
        if is_compressed {
            let raw = fs::read(&file)?;
            let mut decoder = GzDecoder::new(&raw[..]);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            Ok(Dict { data: DictData::Owned(decompressed) })
        } else {
            let f = File::open(&file)?;
            // SAFETY: dictionary files are read-only; we do not modify them.
            let mmap = unsafe { Mmap::map(&f)? };
            Ok(Dict { data: DictData::Mapped(mmap) })
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.data {
            DictData::Mapped(m) => m,
            DictData::Owned(v) => v,
        }
    }

    pub fn data_len(&self) -> usize {
        self.bytes().len()
    }

    pub fn read_entry(
        &self,
        offset: u64,
        size: u32,
        sametypesequence: Option<&str>,
    ) -> Vec<DictEntry> {
        let data = self.bytes();
        let start = offset as usize;
        let end = start + size as usize;
        let mut raw = &data[start..end];
        let mut entries = Vec::new();

        match sametypesequence {
            Some(sts) => {
                let types: Vec<char> = sts.chars().collect();
                for (i, &type_id) in types.iter().enumerate() {
                    let is_last = i == types.len() - 1;
                    if is_last {
                        entries.push(DictEntry {
                            type_id,
                            data: raw.to_vec(),
                        });
                        break;
                    }
                    if type_id.is_ascii_uppercase() {
                        let len =
                            u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
                        entries.push(DictEntry {
                            type_id,
                            data: raw[..null_pos].to_vec(),
                        });
                        raw = &raw[null_pos + 1..];
                    }
                }
            }
            None => {
                while !raw.is_empty() {
                    let type_id = raw[0] as char;
                    raw = &raw[1..];

                    if type_id.is_ascii_uppercase() {
                        let len =
                            u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
                        entries.push(DictEntry {
                            type_id,
                            data: raw[..null_pos].to_vec(),
                        });
                        raw = &raw[null_pos + 1..];
                    }
                }
            }
        }

        entries
    }
}
```

### 5. `src/io.rs` — unchanged

No changes needed. Still used by `Idx` for `.idx.gz` decompression.

### 6. `src/dictionary.rs` — update API calls

Three methods change. `lookup_synonym` uses `idx.entry(i)` instead of
`idx.entries().get(i)`. `word_list` iterates with `word_at`. `search` and
`lookup` are unchanged (they use `idx.search()` which returns owned `IdxEntry`).

```diff
     pub fn lookup_synonym(&self, synonym: &str) -> Option<Vec<DictEntry>> {
         let syn = self.syn.as_ref()?;
         let syn_entry = syn.lookup(synonym)?;
-        let idx_entries = self.idx.entries();
-        let target = idx_entries.get(syn_entry.original_word_index as usize)?;
+        let i = syn_entry.original_word_index as usize;
+        if i >= self.idx.entry_count() {
+            return None;
+        }
+        let target = self.idx.entry(i);
         let sametypesequence = if self.ifo.same_type_sequence.is_empty() {
             None
         } else {
             Some(self.ifo.same_type_sequence.as_str())
         };
         Some(self.dict.read_entry(target.offset, target.size, sametypesequence))
     }

     pub fn word_list(&self) -> Vec<String> {
-        self.idx.entries().iter().map(|e| e.word.clone()).collect()
+        (0..self.idx.entry_count())
+            .map(|i| self.idx.word_at(i).to_string())
+            .collect()
     }
```

### 7. `examples/bench.rs` — update memory reporting

Replace the memory section. The timing section is unchanged except
`idx_entries` is replaced with direct `entry_count()` / `word_at()` calls.

```rust
use std::mem;
use std::path::PathBuf;
use std::time::Instant;

fn fmt_bytes(b: usize) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{} B", b)
    }
}

fn bench_dict(dir: &str, name: &str) {
    let dicts_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("dicts");

    // Bench: load
    let t = Instant::now();
    let dict = stardict::dictionary::Dictionary::open(
        dicts_dir.join(dir), name,
    ).unwrap();
    let load_us = t.elapsed().as_micros();

    let n = dict.idx.entry_count();
    println!("=== {} ({} words) ===", dict.info().name, n);
    println!();

    // --- Memory ---
    let idx_data = dict.idx.data_len();
    let idx_offsets = dict.idx.offsets_len() * mem::size_of::<u32>();
    let idx_total = idx_data + idx_offsets;

    let dict_total = dict.dict.data_len();

    let syn_total = match &dict.syn {
        Some(syn) => {
            syn.data_len() + syn.offsets_len() * mem::size_of::<u32>()
        }
        None => 0,
    };

    let total = idx_total + dict_total + syn_total;

    println!("  memory:");
    println!("    idx:   {:>10}  ({} raw + {} offsets)",
        fmt_bytes(idx_total), fmt_bytes(idx_data), fmt_bytes(idx_offsets));
    println!("    dict:  {:>10}", fmt_bytes(dict_total));
    if let Some(syn) = &dict.syn {
        let syn_data = syn.data_len();
        let syn_offsets = syn.offsets_len() * mem::size_of::<u32>();
        println!("    syn:   {:>10}  ({} raw + {} offsets)",
            fmt_bytes(syn_total), fmt_bytes(syn_data), fmt_bytes(syn_offsets));
    }
    println!("    total: {:>10}", fmt_bytes(total));
    println!();

    // --- Timing ---
    println!("  timing:");
    println!("    load:        {:>8.2} ms", load_us as f64 / 1000.0);

    // Bench: lookup every word
    let t = Instant::now();
    for i in 0..n {
        let w = dict.idx.word_at(i);
        let _ = dict.lookup(w);
    }
    let all_us = t.elapsed().as_micros();
    let per_ns = if n > 0 { (all_us as f64 * 1000.0) / n as f64 } else { 0.0 };
    println!("    lookup all:  {:>8.2} ms  ({:.0} ns/word)", all_us as f64 / 1000.0, per_ns);

    // Bench: lookup misses
    let miss_words: Vec<String> = (0..n.min(10000))
        .map(|i| format!("__miss_{}__", i))
        .collect();
    let t = Instant::now();
    for w in &miss_words {
        let _ = dict.lookup(w);
    }
    let miss_us = t.elapsed().as_micros();
    let miss_per_ns = (miss_us as f64 * 1000.0) / miss_words.len() as f64;
    println!("    miss ({}): {:>8.2} ms  ({:.0} ns/lookup)", miss_words.len(), miss_us as f64 / 1000.0, miss_per_ns);

    // Bench: word_list
    let t = Instant::now();
    let _ = dict.word_list();
    let list_us = t.elapsed().as_micros();
    println!("    word_list:   {:>8.2} ms", list_us as f64 / 1000.0);

    println!();
}

fn main() {
    bench_dict("stardict-xiandaihanyucidian_fix-2.4.2", "xiandaihanyucidian_fix");
    bench_dict("stardict-langdao-ce-gb-2.4.2", "langdao-ce-gb");
    bench_dict("stardict-KoreanEnglishDic-2.4.2", "KoreanEnglishDic");
    bench_dict("stardict-spanish-english-wiktionary", "Spanish-English Wiktionary dictionary");
}
```

### 8. `tests/idx.rs` — update to new API

Replace `idx.entries()` calls with `entry_count()`, `entry()`, `word_at()`.
The `search()` return type changes from `Option<&IdxEntry>` to `Option<IdxEntry>`
but all existing assertions work identically on owned values.

```rust
//! Tests for IDX binary parsing.

extern crate stardict;

use std::path::PathBuf;

use stardict::idx::{Idx, IdxEntry};

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
```

### 9. `tests/syn.rs` — update to new API

Replace `syn.entries()` calls with `entry_count()`, `entry()`, `word_at()`.

```rust
//! Tests for SYN (synonym) file parsing.

extern crate stardict;

use std::path::PathBuf;

use stardict::syn::{Syn, SynEntry};

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
```

### 10. Unchanged files

- `tests/integration.rs` — no changes needed, uses `Dictionary`-level API
- `tests/real_dicts.rs` — no changes needed, uses `word_list()` which keeps same signature
- `tests/strcmp.rs` — unrelated
- `tests/ifo.rs` — unrelated
- `tests/dict.rs` — unrelated
- `src/ifo.rs` — unrelated
- `src/strcmp.rs` — unrelated
- `src/lib.rs` — unrelated

### 11. Other examples — minor fixups

`examples/verify_search.rs`, `examples/debug_search.rs`, `examples/debug_sort.rs`,
`examples/debug_missing.rs` all call `dict.word_list()` which is unchanged, so they
continue to work without modification.

## Implementation Order

1. Add `memmap2` to `Cargo.toml`
2. Replace `src/idx.rs`
3. Replace `src/syn.rs`
4. Replace `src/dict.rs`
5. Update `src/dictionary.rs` (two methods)
6. Replace `tests/idx.rs`
7. Replace `tests/syn.rs`
8. Replace `examples/bench.rs`
9. `cargo test` — verify all 87 tests pass
10. `cargo run --release --example bench` — verify memory reduction

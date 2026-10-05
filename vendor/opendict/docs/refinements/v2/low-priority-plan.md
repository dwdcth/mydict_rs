# Low Priority Fixes — Implementation Plan

Items 8–13 from known-issues.md.

---

## Issue 8: Functions take `PathBuf` where `&Path` suffices

Seven `open()` functions accept owned `PathBuf` but only borrow the path. Changing to `&Path` avoids unnecessary `.to_path_buf()` / `.clone()` at call sites and is idiomatic Rust.

### Signature changes

**`src/stardict/mod.rs` — `StarDictDictionary::open_dir` (line 25)**:
```rust
// Before:
pub fn open_dir(dir: path::PathBuf) -> crate::Result<Self> {
    let ifo_path = find_file_with_ext(&dir, "ifo")?;
// After:
pub fn open_dir(dir: &path::Path) -> crate::Result<Self> {
    let ifo_path = find_file_with_ext(dir, "ifo")?;
```

**`src/stardict/mod.rs` — `StarDictDictionary::open` (line 39)**:
```rust
// Before:
pub fn open(dir: path::PathBuf, name: &str) -> crate::Result<Self> {
    let ifo = ifo::Ifo::open(dir.join(format!("{}.ifo", name)))?;
    Self::open_from_ifo(&dir, name, ifo)
// After:
pub fn open(dir: &path::Path, name: &str) -> crate::Result<Self> {
    let ifo = ifo::Ifo::open(&dir.join(format!("{}.ifo", name)))?;
    Self::open_from_ifo(dir, name, ifo)
```

**`src/stardict/mod.rs` — internal call sites in `open_from_ifo` (lines 34, 51, 59, 63)**:
```rust
// Before:
let ifo = ifo::Ifo::open(ifo_path)?;
...
let idx = idx::Idx::open(idx_path, ifo.idx_offset_bits)?;
...
let dict = dict::Dict::open(dict_path, true)?;
...
Some(syn::Syn::open(syn_path)?)
// After:
let ifo = ifo::Ifo::open(&ifo_path)?;
...
let idx = idx::Idx::open(&idx_path, ifo.idx_offset_bits)?;
...
let dict = dict::Dict::open(&dict_path, true)?;
...
Some(syn::Syn::open(&syn_path)?)
```

**`src/stardict/ifo.rs` — `Ifo::open` (line 23)**:
```rust
// Before:
pub fn open(file: path::PathBuf) -> crate::Result<Ifo> {
// After:
pub fn open(file: &path::Path) -> crate::Result<Ifo> {
```
Body: no changes needed — `fs::File::open` accepts `impl AsRef<Path>`.

**`src/stardict/idx.rs` — `Idx::open` (line 25)**:
```rust
// Before:
pub fn open(file: path::PathBuf, offset_bits: u32) -> crate::Result<Idx> {
    let data = super::io::read_file(&file)?;
// After:
pub fn open(file: &path::Path, offset_bits: u32) -> crate::Result<Idx> {
    let data = super::io::read_file(file)?;
```

**`src/stardict/dict.rs` — `Dict::open` (line 31)**:
```rust
// Before:
pub fn open(file: path::PathBuf, cache_to_disk: bool) -> crate::Result<Dict> {
// After:
pub fn open(file: &path::Path, cache_to_disk: bool) -> crate::Result<Dict> {
```
Body: `&file` references become `file` (already `&Path`).

**`src/stardict/syn.rs` — `Syn::open` (line 23)**:
```rust
// Before:
pub fn open(file: path::PathBuf) -> crate::Result<Syn> {
    let data = fs::read(&file)?;
// After:
pub fn open(file: &path::Path) -> crate::Result<Syn> {
    let data = fs::read(file)?;
```

**`src/mdict/mod.rs` — `MdictDictionary::open` (line 30)**:
```rust
// Before:
pub fn open(dir: path::PathBuf) -> crate::Result<Self> {
    let mdx_path = find_mdx(&dir)?;
// After:
pub fn open(dir: &path::Path) -> crate::Result<Self> {
    let mdx_path = find_mdx(dir)?;
```

### Call site changes

**`src/lib.rs` (lines 24, 29)** — remove `.to_path_buf()`:
```rust
// Before:
let stardict_err = match stardict::StarDictDictionary::open_dir(dir.to_path_buf()) {
...
let mdict_err = match mdict::MdictDictionary::open(dir.to_path_buf()) {
// After:
let stardict_err = match stardict::StarDictDictionary::open_dir(dir) {
...
let mdict_err = match mdict::MdictDictionary::open(dir) {
```

**`tests/integration.rs`** — all `Dictionary::open(fixtures_dir(), ...)` calls: add `&`:
```rust
// Before:
Dictionary::open(fixtures_dir(), "testdict")
// After:
Dictionary::open(&fixtures_dir(), "testdict")
```
Affected: lines 19, 25, 33, 41, 54, 66, 75, 91, 101, 110, 121.

**`tests/real_dicts.rs`** — all `Dictionary::open(dir.clone(), name)` calls: remove `.clone()`:
```rust
// Before:
let dict = Dictionary::open(dir.clone(), name);
// After:
let dict = Dictionary::open(dir, name);
```
Affected: lines 105, 121, 139, 178.

**Test files for internal types** — mechanical change: add `&` to all `open(fixture(...))` and `open(path, ...)` calls:
- `src/stardict/ifo.rs` tests: ~25 call sites → `Ifo::open(&fixture(...))`, `Ifo::open(&path)`
- `src/stardict/idx.rs` tests: ~15 call sites → `Idx::open(&fixture(...), 32)`, `Idx::open(&path, 32)`
- `src/stardict/dict.rs` tests: ~15 call sites → `Dict::open(&fixture(...), false)`, `Dict::open(&path, false)`
- `src/stardict/syn.rs` tests: ~8 call sites → `Syn::open(&fixture(...))`, `Syn::open(&path)`

---

## Issue 9: Deduplicate keyword memory in MDict

`MdictFile` stores keywords three ways:
1. `keywords: Vec<String>` — original case
2. `keyword_map: HashMap<String, usize>` — lowercased keys for lookup
3. `MdictDictionary.sorted_keys: Vec<(String, usize)>` — lowercased keys for prefix search

For 277k-entry dictionaries this triples the string memory. Fix: replace `keyword_map` with a sorted permutation index (`Vec<usize>`) and binary search. Saves ~24 MB for large dictionaries.

### `src/mdict/file.rs` — struct fields

```rust
// Before:
pub struct MdictFile {
    pub header: MdictHeader,
    pub keywords: Vec<String>,
    pub keyword_map: HashMap<String, usize>,
    pub record_offsets: Vec<u64>,
    ...
}

// After:
pub struct MdictFile {
    pub header: MdictHeader,
    pub keywords: Vec<String>,
    sorted_indices: Vec<usize>,
    case_sensitive: bool,
    pub record_offsets: Vec<u64>,
    ...
}
```

Remove `use std::collections::HashMap;`.

### `src/mdict/file.rs` — construction in `open()` (lines 52-61)

```rust
// Before:
let case_sensitive = case_sensitive_override.unwrap_or(header.key_case_sensitive);
let keyword_map: HashMap<String, usize> = keywords
    .iter()
    .enumerate()
    .map(|(i, k)| {
        let key = if case_sensitive { k.clone() } else { k.to_lowercase() };
        (key, i)
    })
    .collect();

// After:
let case_sensitive = case_sensitive_override.unwrap_or(header.key_case_sensitive);
let mut sorted_indices: Vec<usize> = (0..keywords.len()).collect();
if case_sensitive {
    sorted_indices.sort_unstable_by(|&a, &b| keywords[a].cmp(&keywords[b]));
} else {
    let lowercased: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    sorted_indices.sort_unstable_by(|&a, &b| lowercased[a].cmp(&lowercased[b]));
}
```

Update struct literal (line 70):
```rust
// Before:
keyword_map,
// After:
sorted_indices,
case_sensitive,
```

### `src/mdict/file.rs` — `lookup_raw()` (lines 104-108)

```rust
// Before:
let &i = match self.keyword_map.get(key) {
    Some(i) => i,
    None => return Ok(None),
};

// After:
let pos = self.sorted_indices.binary_search_by(|&idx| {
    if self.case_sensitive {
        self.keywords[idx].as_str().cmp(key)
    } else {
        self.keywords[idx].to_lowercase().as_str().cmp(key)
    }
});
let i = match pos {
    Ok(p) => self.sorted_indices[p],
    Err(_) => return Ok(None),
};
```

Note: O(log n) temporary lowercase allocations per lookup (~18 for 277k entries). Negligible cost.

### `src/mdict/file.rs` — test `keyword_map_contains_all` → `keyword_lookup_finds_all`

```rust
// Before:
fn keyword_map_contains_all() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    assert!(mdx.keyword_map.contains_key("foo"));
    assert!(mdx.keyword_map.contains_key("hello"));
    assert!(mdx.keyword_map.contains_key("test"));
}

// After:
fn keyword_lookup_finds_all() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    assert!(mdx.lookup_raw("foo").unwrap().is_some());
    assert!(mdx.lookup_raw("hello").unwrap().is_some());
    assert!(mdx.lookup_raw("test").unwrap().is_some());
}
```

### Follow-up (not in this pass)

`MdictDictionary.sorted_keys` is yet another lowercased copy used for prefix search. Could be consolidated with `MdictFile.sorted_indices` to eliminate the third copy, but increases code complexity. Defer to a future pass.

---

## Issue 10: `word_list()` returns `Vec<&str>` instead of `Vec<String>`

Avoids cloning every keyword string on each call. StarDict's `word_at()` already returns `&str`; MDict can return `&str` refs into `keywords`.

### `src/lib.rs` — trait definition (line 14)

```rust
// Before:
fn word_list(&self) -> Vec<String>;
// After:
fn word_list(&self) -> Vec<&str>;
```

### `src/stardict/mod.rs` — inherent method (line 116) and trait impl (line 132)

```rust
// Before (inherent):
pub fn word_list(&self) -> Vec<String> {
    (0..self.idx.entry_count())
        .map(|i| self.idx.word_at(i).to_string())
        .collect()
}
// After:
pub fn word_list(&self) -> Vec<&str> {
    (0..self.idx.entry_count())
        .map(|i| self.idx.word_at(i))
        .collect()
}

// Before (trait):
fn word_list(&self) -> Vec<String> {
// After:
fn word_list(&self) -> Vec<&str> {
```

### `src/mdict/mod.rs` — trait impl (line 133)

```rust
// Before:
fn word_list(&self) -> Vec<String> {
    self.mdx.keywords.clone()
}
// After:
fn word_list(&self) -> Vec<&str> {
    self.mdx.keywords.iter().map(String::as_str).collect()
}
```

### Test/example call sites

No changes needed — `Vec<&str>` compares with `vec!["literal"]` the same way, and `&words[i]` (yielding `&&str`) auto-derefs to `&str` for `dict.lookup(word)`.

---

## Issue 11: Extract shared code from Idx and Syn

Both have identical `word_at()` and `binary_search()` implementations operating on `data: Vec<u8>` + `offsets: Vec<u32>`. Extract to a shared utility module.

### New file: `src/stardict/index_util.rs`

```rust
use std::cmp::Ordering;
use std::str;

/// Extract a null-terminated UTF-8 word from raw index data.
pub(crate) fn word_at(data: &[u8], offsets: &[u32], i: usize) -> &str {
    let start = offsets[i] as usize;
    let null_pos = data[start..].iter().position(|&b| b == 0).unwrap();
    str::from_utf8(&data[start..start + null_pos]).unwrap()
}

/// Binary search for an exact word match. Returns the matching index.
pub(crate) fn find_match<F>(
    data: &[u8],
    offsets: &[u32],
    word: &str,
    cmp: F,
) -> Option<usize>
where
    F: Fn(&str, &str) -> Ordering,
{
    let mut low = 0usize;
    let mut high = offsets.len();
    while low < high {
        let mid = low + (high - low) / 2;
        match cmp(word_at(data, offsets, mid), word) {
            Ordering::Equal => return Some(mid),
            Ordering::Less => low = mid + 1,
            Ordering::Greater => high = mid,
        }
    }
    None
}
```

### `src/stardict/mod.rs` — register module

```rust
// Add:
pub(crate) mod index_util;
```

### `src/stardict/idx.rs` — delegate to shared code

```rust
// Before:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    str::from_utf8(&self.data[start..start + null_pos]).unwrap()
}

// After:
pub(crate) fn word_at(&self, i: usize) -> &str {
    super::index_util::word_at(&self.data, &self.offsets, i)
}

// Before:
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

// After:
fn binary_search<F>(&self, word: &str, cmp: F) -> Option<IdxEntry>
where
    F: Fn(&str, &str) -> std::cmp::Ordering,
{
    super::index_util::find_match(&self.data, &self.offsets, word, cmp)
        .map(|i| self.entry(i))
}
```

### `src/stardict/syn.rs` — delegate to shared code

```rust
// Before:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    str::from_utf8(&self.data[start..start + null_pos]).unwrap()
}

// After:
pub(crate) fn word_at(&self, i: usize) -> &str {
    super::index_util::word_at(&self.data, &self.offsets, i)
}

// Before:
fn binary_search<F>(&self, word: &str, cmp: F) -> Option<SynEntry>
where
    F: Fn(&str, &str) -> Ordering,
{
    let mut low = 0usize;
    let mut high = self.offsets.len();
    while low < high {
        let mid = low + (high - low) / 2;
        match cmp(self.word_at(mid), word) {
            Ordering::Equal => return Some(self.entry(mid)),
            Ordering::Less => low = mid + 1,
            Ordering::Greater => high = mid,
        }
    }
    None
}

// After:
fn binary_search<F>(&self, word: &str, cmp: F) -> Option<SynEntry>
where
    F: Fn(&str, &str) -> Ordering,
{
    super::index_util::find_match(&self.data, &self.offsets, word, cmp)
        .map(|i| self.entry(i))
}
```

The `str` import can also be removed from both files (it was only used in `word_at`).

---

## Issue 12: Consistent word count types

`DictInfo.word_count` is `u64` but `Dictionary::word_count()` returns `usize`. Since counts are bounded by `Vec` capacity (`usize`), standardise on `usize`.

### `src/types.rs` (line 20)

```rust
// Before:
pub word_count: u64,
// After:
pub word_count: usize,
```

### `src/stardict/ifo.rs` — fields and parsing

```rust
// Before (lines 17-18):
pub word_count: u64,
pub syn_word_count: u64,

// After:
pub word_count: usize,
pub syn_word_count: usize,
```

No changes to the parse logic — `val.parse::<usize>()` works the same.

### `src/mdict/mod.rs` (line 46) — remove `as u64` cast

```rust
// Before:
word_count: mdx.keywords.len() as u64,
// After:
word_count: mdx.keywords.len(),
```

### `tests/real_dicts.rs` (line 124) — remove `as u64` cast

```rust
// Before:
words.len() as u64,
// After:
words.len(),
```

### `src/stardict/ifo.rs` test `wordcount_is_u32_not_isize` — rename

```rust
// Before:
fn wordcount_is_u32_not_isize() {
// After:
fn wordcount_handles_large_values() {
```

---

## Issue 13: Hand-written binary search — Won't Fix

The binary search in `Idx` and `Syn` operates on indices (not slice elements) and uses three-way comparison with early return on `Equal`. `partition_point` only supports monotonic predicates and doesn't short-circuit on match. The manual approach is appropriate here. Already partially addressed by Issue 11 (shared implementation).

---

## Execution Order

1. **Issue 11** (extract shared code) — creates the utility module, reduces code before other changes touch the same files
2. **Issue 8** (`PathBuf` → `&Path`) — foundational API change, many call sites
3. **Issue 12** (consistent types) — small, independent
4. **Issue 10** (`word_list` → `Vec<&str>`) — trait signature change
5. **Issue 9** (deduplicate MDict memory) — isolated to mdict module

## Files Modified

| File | Issues |
|------|--------|
| `src/lib.rs` | 8 (call sites), 10 (trait) |
| `src/types.rs` | 12 (word_count type) |
| `src/stardict/mod.rs` | 8 (signatures), 10 (word_list), 11 (register module) |
| `src/stardict/index_util.rs` | 11 (new file) |
| `src/stardict/idx.rs` | 8 (signature), 11 (delegate) |
| `src/stardict/syn.rs` | 8 (signature), 11 (delegate) |
| `src/stardict/dict.rs` | 8 (signature) |
| `src/stardict/ifo.rs` | 8 (signature), 12 (field types) |
| `src/mdict/mod.rs` | 8 (signature), 10 (word_list), 12 (remove cast) |
| `src/mdict/file.rs` | 9 (keyword dedup) |
| `tests/integration.rs` | 8 (call sites) |
| `tests/real_dicts.rs` | 8 (call sites), 12 (remove cast) |

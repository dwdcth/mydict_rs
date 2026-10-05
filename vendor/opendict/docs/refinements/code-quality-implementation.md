# Code Quality Implementation

Three remaining items from `known-issues.md`: #9, #10, #12.

---

## 9. Use binary search in Syn::lookup()

`src/stardict/syn.rs:77-81` does O(n) linear scan through all entries. Entries are sorted by `stardict_strcmp` (same as idx), so we can binary search.

### Current code

```rust
pub fn lookup(&self, word: &str) -> Option<SynEntry> {
    (0..self.offsets.len())
        .find(|&i| self.word_at(i) == word)
        .map(|i| self.entry(i))
}
```

### New code

```rust
pub fn lookup(&self, word: &str) -> Option<SynEntry> {
    self.binary_search(word, |w, target| stardict_strcmp(w, target))
        .or_else(|| {
            self.binary_search(word, |w, target| w.as_bytes().cmp(target.as_bytes()))
        })
}

fn binary_search<F>(&self, word: &str, cmp: F) -> Option<SynEntry>
where
    F: Fn(&str, &str) -> std::cmp::Ordering,
{
    let mut low = 0usize;
    let mut high = self.offsets.len();
    while low < high {
        let mid = low + (high - low) / 2;
        match cmp(self.word_at(mid), word) {
            std::cmp::Ordering::Equal => return Some(self.entry(mid)),
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
        }
    }
    None
}
```

### Changes

- `src/stardict/syn.rs` — add `use super::strcmp::stardict_strcmp;`, replace `lookup`, add `binary_search`
- Update test `entries_are_sorted` to use `stardict_strcmp` instead of `<=`

---

## 10. Replace f32 version comparisons in MDict

Version is parsed as `f32` and compared with `>= 3.0` / `< 3.0`. Works in practice but fragile. Replace with an enum.

### New type in `src/mdict/header.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdictVersion {
    V2,
    V3,
}
```

### Changes to `src/mdict/header.rs`

1. Add the enum above.
2. Change `MdictHeader.version` from `f32` to `MdictVersion`.
3. In `parse_header`, parse the version string then convert:

```rust
// Before:
let mut version = 2.0f32;
// ...
"GeneratedByEngineVersion" => {
    version = val.parse().map_err(|e| { ... })?;
}

// After:
let mut version_raw = 2.0f32;
// ...
"GeneratedByEngineVersion" => {
    version_raw = val.parse().map_err(|e| { ... })?;
}
// ... after loop:
let version = if version_raw >= 3.0 {
    MdictVersion::V3
} else {
    MdictVersion::V2
};
```

### Changes to consumers

All functions that take `version: f32` change to `version: MdictVersion`.

**`src/mdict/decompress.rs`** — `decompress_block`:
```rust
// Before:
pub fn decompress_block(block: &[u8], version: f32, ...) -> crate::Result<Vec<u8>> {
    // ...
    if version >= 3.0 { ... }
    // ...
    if version < 3.0 { ... }

// After:
pub fn decompress_block(block: &[u8], version: MdictVersion, ...) -> crate::Result<Vec<u8>> {
    // ...
    if version == MdictVersion::V3 { ... }
    // ...
    if version == MdictVersion::V2 { ... }
```

Add `use super::header::MdictVersion;` to imports.

Update tests — `2.0` becomes `MdictVersion::V2`:
```rust
// Before:
decompress_block(&[0; 4], 2.0, None);
// After:
decompress_block(&[0; 4], MdictVersion::V2, None);
```

**`src/mdict/keygen.rs`** — `derive_key`:
```rust
// Before:
pub fn derive_key(version: f32, uuid: Option<&[u8]>) -> Option<[u8; 16]> {
    if version >= 3.0 { ... } else { None }

// After:
pub fn derive_key(version: MdictVersion, uuid: Option<&[u8]>) -> Option<[u8; 16]> {
    if version == MdictVersion::V3 { ... } else { None }
```

Add `use super::header::MdictVersion;` to imports.

Update tests — `2.0` becomes `MdictVersion::V2`, `3.0` becomes `MdictVersion::V3`:
```rust
// Before:
derive_key(2.0, Some(b"anything"))
// After:
derive_key(MdictVersion::V2, Some(b"anything"))
```

**`src/mdict/file.rs`** — no signature changes, just passes `self.header.version` which is now `MdictVersion`. No edits needed.

**`src/mdict/keys.rs`** — no signature changes, passes `header.version` through. No edits needed.

**`tests/mdict_fixture.rs`** — version assertion:
```rust
// Before:
assert!((mdx.header.version - 2.0).abs() < f32::EPSILON);
// After:
assert_eq!(mdx.header.version, opendict::mdict::header::MdictVersion::V2);
```

Actually, `MdictVersion` needs to be pub-reachable from the test. `header` is already `pub mod header` in `src/mdict/mod.rs` and `mdict` is `pub mod mdict` in `src/lib.rs`, so `opendict::mdict::header::MdictVersion` works.

---

## 12. Dict::data_len() returns 0 for mmapped files

`src/stardict/dict.rs:74-76` returns 0 for the `Mapped` variant. Should return the actual mmap length.

### Current code

```rust
pub fn data_len(&self) -> usize {
    match &self.data {
        DictData::Mapped(_) => 0,
        DictData::Owned(v) => v.len(),
    }
}
```

### New code

```rust
pub fn data_len(&self) -> usize {
    self.bytes().len()
}
```

This delegates to `bytes()` which already handles both variants correctly. Single line change.

---

## File change summary

| File | Change |
|------|--------|
| `src/stardict/syn.rs` | Add import, replace `lookup` with binary search, add `binary_search` method, fix test |
| `src/mdict/header.rs` | Add `MdictVersion` enum, change `version` field type, convert at parse time |
| `src/mdict/decompress.rs` | Change parameter type, update comparisons, update tests |
| `src/mdict/keygen.rs` | Change parameter type, update comparisons, update tests |
| `src/stardict/dict.rs` | Fix `data_len()` one-liner |
| `tests/mdict_fixture.rs` | Update version assertion |

## Verification

```
cargo check
cargo test
cargo bench --no-run
```

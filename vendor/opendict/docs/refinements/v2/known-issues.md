# Known Issues — v2

## High Priority

### 1. Error swallowing in `open()`
`lib.rs:23-26` — when StarDict parsing fails (permission denied, corrupt file), the error is silently discarded and falls through to MDict. Users get a confusing "no .mdx file found" error instead of the real failure. Should distinguish "wrong format" from genuine I/O errors.

### 2. Premature 1.0 with overly wide public API
`version = "1.0.0"` in Cargo.toml but all internal modules (`stardict::ifo`, `stardict::idx`, `mdict::file`, etc.) are `pub`, struct fields like `MdictFile.header`, `MdictFile.keywords`, `Ifo.author` etc. are `pub`, and no types use `#[non_exhaustive]`. Any internal restructuring is semver-breaking.

## Medium Priority

### 3. Unnecessary `unsafe` in Idx and Syn
`idx.rs:64` and `syn.rs:59` use `str::from_utf8_unchecked`. Correctness depends on validation during `open()` — fragile if `open()` is later modified. Performance gain is negligible for dictionary lookups.

### 4. 13 clippy warnings
- `map_or(false, ...)` → `is_some_and()` (4 instances: `dict.rs:30`, `io.rs:10`, `mod.rs:152,156`)
- Redundant closures around `stardict_strcmp` (`idx.rs:107`, `syn.rs:80`)
- Manual range check `b >= b'A' && b <= b'Z'` (`strcmp.rs:36`)
- Manual `div_ceil` (`keygen.rs:9`)
- Manual bit rotation (`ripemd128.rs:80`, `decrypt.rs:12`)
- Complex return type (`records.rs:9`)
- Empty line after doc comment (`ripemd128.rs:3`)
- Single-arm match → `if let` (`lib.rs:23`)
- Unnecessary cast (`real_dicts.rs:125`)

### 5. Missing `#[non_exhaustive]` on public types
`DictEntry`, `DictInfo`, `Error` lack `#[non_exhaustive]`. Adding a field/variant later is semver-breaking.

### 6. Dead parameter `Syn::open(_syn_word_count)`
Unused parameter in `syn.rs:21`, suppressed with `_` prefix. Should be removed from signature and call site.

### 7. Missing `Debug` on public types
`StarDictDictionary`, `MdictDictionary`, `MdictFile`, `Dict`, `Idx`, `Syn` don't implement `Debug`.

## Low Priority

### 8. Functions take `PathBuf` where `&Path` suffices
`StarDictDictionary::open_dir`, `open`, `MdictDictionary::open`, `Ifo::open`, `Idx::open`, `Dict::open`, `Syn::open` all take owned `PathBuf` but only read the path.

### 9. Duplicated keyword memory in MDict
`MdictFile` stores keywords in both `Vec<String>` and `HashMap<String, usize>`. Doubles string memory for large dictionaries (277k entries = significant).

### 10. `word_list()` allocates on every call
Trait returns `Vec<String>`, forcing a full clone. An iterator approach would be zero-allocation.

### 11. Code duplication between `Idx` and `Syn`
Nearly identical structure: `data: Vec<u8>` + `offsets: Vec<u32>`, same `word_at()`, same `binary_search()`, same `open()` parse loop.

### 12. Type inconsistency for word counts
`DictInfo.word_count` is `u64`, `Dictionary::word_count()` returns `usize`, `Ifo.syn_word_count` is `u64` cast to `u32`. Silent truncation on 32-bit targets.

### 13. Hand-written binary search in Idx and Syn
Manual binary search where `partition_point` (already used by MDict) would be more idiomatic.

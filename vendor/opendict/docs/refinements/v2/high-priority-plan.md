# High Priority Fixes — Implementation Plan

## Issue 1: Error swallowing in `open()`

### Problem
`lib.rs:23-26` silently discards StarDict errors. A permission-denied or corrupt-file error gets masked by a subsequent "no .mdx file found" MDict error.

### Solution
Distinguish "not this format" from real failures. If StarDict fails with `Io` or a definitive parsing error (not just "no .ifo file found"), propagate immediately rather than falling through to MDict.

### Changes

**`src/lib.rs`** — replace the match block:

```rust
// Before:
match stardict::StarDictDictionary::open_dir(dir.to_path_buf()) {
    Ok(dict) => return Ok(Box::new(dict)),
    Err(_) => {}
}
let dict = mdict::MdictDictionary::open(dir.to_path_buf())?;
Ok(Box::new(dict))

// After:
let stardict_err = match stardict::StarDictDictionary::open_dir(dir.to_path_buf()) {
    Ok(dict) => return Ok(Box::new(dict)),
    Err(e) => e,
};

let mdict_err = match mdict::MdictDictionary::open(dir.to_path_buf()) {
    Ok(dict) => return Ok(Box::new(dict)),
    Err(e) => e,
};

// Both formats failed — return the more informative error.
// If one was just "wrong format" and the other was a real parse/IO error, prefer the real one.
// If both are format-detection failures, combine them.
Err(Error::InvalidFormat(format!(
    "not a recognized dictionary: StarDict: {}; MDict: {}",
    stardict_err, mdict_err
)))
```

This gives the user both errors so they can see what actually went wrong.

---

## Issue 2: Premature 1.0 with overly wide public API

### Problem
The crate is `1.0.0` but exposes every internal module and struct field. Any refactor is a semver-breaking change.

### Sub-problem 2a: Version
### Sub-problem 2b: Module visibility
### Sub-problem 2c: `#[non_exhaustive]`

### Changes

#### 2a. Cargo.toml — downgrade to 0.1.0

```toml
# Before:
version = "1.0.0"

# After:
version = "0.1.0"
```

#### 2b. Module visibility — make internal modules `pub(crate)`

**`src/lib.rs`**:
```rust
// Before:
pub mod stardict;
pub mod mdict;

// After:
pub mod stardict;
pub mod mdict;
```

Actually, external tests (`tests/mdict_fixture.rs`, `tests/integration.rs`) import `opendict::mdict::file::MdictFile` and `opendict::mdict::header::MdictVersion` directly. We need to keep `mdict` and `stardict` themselves `pub` for now but restrict the sub-modules.

**`src/stardict/mod.rs`** — hide internal modules:
```rust
// Before:
pub mod ifo;
pub mod idx;
pub mod dict;
pub mod syn;
pub mod strcmp;
pub mod io;

// After:
pub(crate) mod ifo;
pub(crate) mod idx;
pub(crate) mod dict;
pub(crate) mod syn;
pub(crate) mod strcmp;
pub(crate) mod io;
```

**`src/mdict/mod.rs`** — hide internal modules, re-export what tests need:
```rust
// Before:
pub mod header;
pub mod keys;
pub mod records;
pub mod decompress;
pub mod ripemd128;
pub mod decrypt;
pub mod encoding;
pub mod keygen;
pub mod file;

// After:
pub(crate) mod header;
pub(crate) mod keys;
pub(crate) mod records;
pub(crate) mod decompress;
pub(crate) mod ripemd128;
pub(crate) mod decrypt;
pub(crate) mod encoding;
pub(crate) mod keygen;
pub(crate) mod file;
```

This breaks `tests/mdict_fixture.rs` which imports `opendict::mdict::file::MdictFile` and `opendict::mdict::header::MdictVersion`. Fix by testing through the public `open()` API or adding targeted re-exports.

**Fixture test migration** — `tests/mdict_fixture.rs` currently tests `MdictFile` directly. The low-level tests (header version, encoding, format, keyword count, record blocks) should move to `#[cfg(test)]` inside `src/mdict/file.rs`. The integration-level tests (full_dictionary_lookup, full_dictionary_word_list, etc.) already use `opendict::open()` and stay as-is.

Steps:
1. Move unit-level MdictFile tests from `tests/mdict_fixture.rs` into `src/mdict/file.rs` as `#[cfg(test)] mod tests`.
2. Keep only the `full_dictionary_*` tests in `tests/mdict_fixture.rs`, which use `opendict::open()`.
3. The remaining fixture tests in `tests/mdict_fixture.rs` that test `lookup_raw` etc. should become in-crate tests.

**`MdictFile` field visibility** — make fields `pub(crate)`:
```rust
// src/mdict/file.rs — Before:
pub struct MdictFile {
    pub header: MdictHeader,
    pub keywords: Vec<String>,
    pub keyword_map: HashMap<String, usize>,
    pub record_offsets: Vec<u64>,
    pub data: Mmap,
    pub record_blocks: Vec<(u64, u64, u64)>,
    pub record_blocks_start: u64,
    pub global_key: Option<[u8; 16]>,
    ...
}

// After:
pub(crate) struct MdictFile {
    pub(crate) header: MdictHeader,
    pub(crate) keywords: Vec<String>,
    pub(crate) keyword_map: HashMap<String, usize>,
    pub(crate) record_offsets: Vec<u64>,
    pub(crate) data: Mmap,
    pub(crate) record_blocks: Vec<(u64, u64, u64)>,
    pub(crate) record_blocks_start: u64,
    pub(crate) global_key: Option<[u8; 16]>,
    ...
}
```

Same for `MdictHeader` in `src/mdict/header.rs`:
```rust
// Before:
pub struct MdictHeader {
    pub version: MdictVersion,
    pub encoding: String,
    ...
}

// After:
pub(crate) struct MdictHeader {
    pub(crate) version: MdictVersion,
    pub(crate) encoding: String,
    ...
}
```

And `MdictVersion`:
```rust
// Before:
pub enum MdictVersion { V2, V3 }

// After:
pub(crate) enum MdictVersion { V2, V3 }
```

**`Ifo` field visibility**:
```rust
// src/stardict/ifo.rs — all pub fields become pub(crate):
pub(crate) struct Ifo {
    pub(crate) author: String,
    pub(crate) version: String,
    ...
}
```

`StarDictDictionary` already has `pub(crate)` fields (done in v1), and exposes `ifo()` as a getter. But `ifo()` returns `&Ifo` which exposes `Ifo`'s public fields. With `Ifo` becoming `pub(crate)`, the `ifo()` method should be removed or return a `&DictInfo` instead. The test in `tests/integration.rs:88-96` (`dict.ifo()`) needs updating to use `dict.info()` instead.

**`tests/integration.rs` update**:
```rust
// Before:
fn info_returns_correct_metadata() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let ifo = dict.ifo();
    assert_eq!(ifo.name, "A foo-bar dictionary");
    assert_eq!(ifo.version, "3.0.0");
    assert_eq!(ifo.word_count, 4);
    assert_eq!(ifo.idx_file_size, 60);
    assert_eq!(ifo.same_type_sequence, "m");
}

// After:
fn info_returns_correct_metadata() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let info = dict.info();
    assert_eq!(info.name, "A foo-bar dictionary");
    assert_eq!(info.word_count, 4);
}
```

The `version`, `idx_file_size`, and `same_type_sequence` fields are StarDict-specific internals that shouldn't be exposed through the unified API. They can be tested in the unit tests inside `ifo.rs` (which already test them).

#### 2c. `#[non_exhaustive]` on public types

**`src/types.rs`**:
```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DictInfo {
    pub name: String,
    pub author: String,
    pub description: String,
    pub word_count: u64,
}
```

`#[non_exhaustive]` on structs prevents external construction via struct literals. Internal code needs updating to use a constructor or remain in-crate. Since our crate constructs these in `stardict/mod.rs` and `mdict/mod.rs`, in-crate code is unaffected. But we should provide a way for users to inspect the fields (the `pub` fields are still accessible, just can't be used in struct literal construction from outside).

**`src/error.rs`**:
```rust
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(std::io::Error),
    InvalidFormat(String),
    Unsupported(String),
}
```

---

## Execution Order

1. **Fix `open()` error handling** (Issue 1) — single file change, no API impact
2. **Add `#[non_exhaustive]`** (Issue 2c) — small addition, no breakage for current users
3. **Restrict module/field visibility** (Issue 2b) — requires moving tests, most involved change
4. **Downgrade version** (Issue 2a) — trivial, do last since it's a statement about API stability

## Files Modified

| File | Changes |
|------|---------|
| `src/lib.rs` | Fix `open()` error handling |
| `src/types.rs` | Add `#[non_exhaustive]` |
| `src/error.rs` | Add `#[non_exhaustive]` |
| `Cargo.toml` | Version → `0.1.0` |
| `src/stardict/mod.rs` | Modules → `pub(crate)`, remove `ifo()` getter |
| `src/stardict/ifo.rs` | Fields → `pub(crate)` |
| `src/mdict/mod.rs` | Modules → `pub(crate)` |
| `src/mdict/file.rs` | Struct/fields → `pub(crate)`, add `#[cfg(test)]` for moved tests |
| `src/mdict/header.rs` | Struct/enum/fields → `pub(crate)` |
| `tests/mdict_fixture.rs` | Move unit tests to `src/mdict/file.rs`, keep only integration tests |
| `tests/integration.rs` | Replace `dict.ifo()` with `dict.info()` |

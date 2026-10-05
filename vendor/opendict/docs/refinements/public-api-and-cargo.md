# Public API & Cargo.toml Cleanup

Covers known-issues #1, #2, #4, #5, #15, #16, plus LICENSE update.

(#3 auto-detect already done.)

## Summary

| File | Change |
|---|---|
| `src/lib.rs` | Remove legacy re-exports, remove `DictKind`, re-export `DictEntry`/`DictInfo` at root |
| `src/stardict/mod.rs` | Make fields `pub(crate)`, fix duplicate `word_list()` |
| `tests/integration.rs` | Switch from `dictionary::Dictionary` alias to `stardict::StarDictDictionary` |
| `tests/real_dicts.rs` | Same import fix |
| `Cargo.toml` | Fix authors, add `repository`/`keywords`/`categories`/`rust-version` |
| `LICENSE` | Update copyright line |

## 1. `src/lib.rs`

Replace the entire file:

```rust
pub mod types;
pub mod stardict;
pub mod mdict;

use std::path;

pub use types::{DictEntry, DictInfo};

pub trait Dictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn lookup_synonym(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn word_list(&self) -> Vec<String>;
    fn word_count(&self) -> usize;
    fn info(&self) -> DictInfo;
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;
}

pub fn open(dir: impl AsRef<path::Path>) -> anyhow::Result<Box<dyn Dictionary + Send + Sync>> {
    let dir = dir.as_ref();
    // Try StarDict first (.ifo), then MDict (.mdx)
    match stardict::StarDictDictionary::open_dir(dir.to_path_buf()) {
        Ok(dict) => return Ok(Box::new(dict)),
        Err(_) => {}
    }
    let dict = mdict::MdictDictionary::open(dir.to_path_buf())?;
    Ok(Box::new(dict))
}
```

What's removed:
- `DictKind` enum — dead code, `open()` auto-detects now. `node/` and `mobile/` bindings reference it but they already broke when `open()` changed; they need a separate update.
- Root re-exports of `ifo`, `idx`, `dict`, `syn`, `strcmp`, `io` — leaked internals.
- `dictionary` module alias — confusing indirection to `StarDictDictionary`.

What's added:
- `pub use types::{DictEntry, DictInfo}` — these are the public data types callers need.

## 2. `src/stardict/mod.rs`

Two changes:

### a) Make struct fields `pub(crate)`

Nothing outside the crate needs direct access to `idx`, `ifo`, `dict`, `syn` fields anymore (the old `bench.rs` that accessed them is deleted).

```rust
pub struct StarDictDictionary {
    pub(crate) idx: idx::Idx,
    pub(crate) ifo: ifo::Ifo,
    pub(crate) dict: dict::Dict,
    pub(crate) syn: Option<syn::Syn>,
}
```

### b) Fix duplicate `word_list()`

The trait impl on line 117 duplicates the inherent impl on line 101. Replace the trait impl body with delegation:

```rust
impl Dictionary for StarDictDictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>> {
        self.lookup(word)
    }

    fn lookup_synonym(&self, word: &str) -> Option<Vec<DictEntry>> {
        self.lookup_synonym(word)
    }

    fn word_list(&self) -> Vec<String> {
        self.word_list()
    }

    fn word_count(&self) -> usize {
        self.idx.entry_count()
    }

    fn info(&self) -> DictInfo {
        DictInfo {
            name: self.ifo.name.clone(),
            author: self.ifo.author.clone(),
            description: self.ifo.description.clone(),
            word_count: self.ifo.word_count,
        }
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.idx.search_prefix(prefix, limit)
    }
}
```

(Only `word_list()` body changed — was duplicated, now delegates.)

## 3. `tests/integration.rs`

Replace the import. The tests use `StarDictDictionary::open(dir, name)` which takes a directory + dictionary name prefix — this is the StarDict-specific API, not the generic `opendict::open()`.

```rust
// Old:
use opendict::dictionary::Dictionary;

// New:
use opendict::stardict::StarDictDictionary as Dictionary;
```

One-line change; all test code stays the same since `Dictionary` is still the local name.

Note: `info_returns_correct_metadata` accesses `info.version`, `info.idx_file_size`, `info.same_type_sequence` which are `Ifo` fields (returned by the inherent `info()` method, not the trait). This keeps working because the tests use the concrete type.

## 4. `tests/real_dicts.rs`

Same import fix:

```rust
// Old:
use opendict::dictionary::Dictionary;

// New:
use opendict::stardict::StarDictDictionary as Dictionary;
```

Only the StarDict tests use this. MDict tests already use `opendict::open()`.

## 5. `Cargo.toml`

Replace the full `[package]` section:

```toml
[package]
name = "opendict-rs"
version = "1.0.0"
edition = "2024"
rust-version = "1.85"
authors = ["Callum Gander <callumgander@protonmail.com>", "Jeremy Zheng <jitang.zheng@gmail.com>"]
description = "Rust implementation of a unified reader for both StarDict and MDict"
homepage = "https://github.com/callum/opendict-rs"
repository = "https://github.com/callum/opendict-rs"
license = "MIT"
keywords = ["stardict", "mdict", "dictionary"]
categories = ["parser-implementations", "text-processing"]
```

Changes:
- Fixed malformed first author (missing closing `>` and `"`).
- Added `rust-version = "1.85"` (minimum for edition 2024).
- Added `repository` (same as homepage).
- Added `keywords` and `categories`.
- Capitalised "StarDict" in description.

## 6. `LICENSE`

Replace the copyright line:

```
MIT License

Copyright (c) 2025 Callum Gander, Jeremy Zheng
```

(Rest of the file unchanged.)

## Notes

- `node/` and `mobile/` bindings reference the old `opendict::DictKind` and `opendict::open(dir, kind)` API. They need a separate update to use `opendict::open(dir)` with auto-detect instead.
- `tests/mdict_fixture.rs` imports `opendict::mdict::file::MdictFile` directly. This still works since `mdict` stays `pub`. A future cleanup could move those low-level tests into the mdict source module.
- Known-issue #2 (take `impl AsRef<Path>`) is done for `open()`. The internal functions (`Ifo::open`, `Idx::open`, etc.) still take `PathBuf` — not a public API concern since callers go through `opendict::open()`.

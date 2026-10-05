# Known Issues — All Resolved

## ~~Public API~~

### ~~1. Remove legacy re-exports from lib.rs~~
Done. Removed root re-exports of `ifo`, `idx`, `dict`, `syn`, `strcmp`, `io` and the `dictionary` module alias. `DictEntry` and `DictInfo` re-exported at root. `StarDictDictionary` fields made `pub(crate)`.

### ~~2. Take &Path / impl AsRef<Path> instead of PathBuf~~
Done for `open()`. Internal functions still take `PathBuf` — not a public API concern.

### ~~3. Auto-detect dictionary format~~
Done. `open()` now takes a single path, tries StarDict then MDict. `DictKind` removed.

### ~~4. Add derives to DictKind~~
Moot — `DictKind` removed (dead code after #3).

### ~~5. Fix duplicate word_list() implementation~~
Done. Trait impl now delegates to the inherent method.

## ~~Error Handling~~

### ~~6. Replace anyhow with a custom error type~~
Done. Three-variant `Error` enum (`Io`, `InvalidFormat`, `Unsupported`) in `src/error.rs`. `anyhow` removed from runtime deps.

### ~~7. Remove eprintln! from library code~~
Done. Replaced with `log::warn!` in `src/mdict/mod.rs`.

### ~~8. Bounds-check slice indexing in Dict::read_entry~~
Done. `Dict::read_entry()` now returns `Err(InvalidFormat(...))` if offset+size exceeds data length.

## ~~Code Quality~~

### ~~9. Use binary search in Syn::lookup()~~
Done. Binary search using `stardict_strcmp` with byte-level fallback, matching the pattern in `Idx::search()`.

### ~~10. Replace f32 version comparisons in MDict~~
Done. `MdictVersion::V2`/`V3` enum in `header.rs`, used across `decompress.rs`, `keygen.rs`, and consumers.

### ~~11. Remove unused parameter in Idx::open()~~
Done. `_word_count` parameter removed from `Idx::open()` signature and all call sites.

### ~~12. Dict::data_len() returns 0 for mmapped files~~
Done. Delegates to `self.bytes().len()` which handles both variants.

## ~~Examples & Benchmarks~~

### ~~13. Examples should take CLI args~~
Done. Replaced with `examples/lookup.rs` and `examples/show_dict.rs` (both take CLI args). `verify_search.rs` logic moved to `tests/real_dicts.rs`.

### ~~14. Move bench.rs to benches/ with criterion or divan~~
Done. Replaced with `benches/dictionary.rs` using criterion. Benchmarks driven by `OPENDICT_BENCH_DIR` env var.

## ~~Cargo.toml~~

### ~~15. Fix malformed authors field~~
Done. Fixed closing `>` and `"`.

### ~~16. Add missing metadata~~
Done. Added `repository`, `keywords`, `categories`, `rust-version = "1.85"`. Updated LICENSE copyright.

## Resolved

### ~~Remove unnecessary extern crate declarations~~
Removed from `tests/integration.rs` and `tests/real_dicts.rs`.

### ~~Move internal tests out of tests/ into source modules~~
Moved tests from `tests/dict.rs`, `tests/idx.rs`, `tests/ifo.rs`, `tests/syn.rs`, `tests/strcmp.rs` into `#[cfg(test)]` modules inside their respective source files. Deleted the old test files.

### ~~Remove debug example scripts~~
Deleted `examples/debug_missing.rs`, `examples/debug_search.rs`, `examples/debug_sort.rs`.

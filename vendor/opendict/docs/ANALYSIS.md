# StarDict Reader — Current State Analysis

## Summary

This is a **skeleton/proof-of-concept**. The actual StarDict reading logic is almost entirely unimplemented. Roughly 10-15% of a working reader exists.

## What Exists

- **IFO parsing** (`src/ifo.rs`): Partially works. Reads key=value pairs from `.ifo` files.
- **Directory scanning** (`src/lib.rs`): Scans a root directory for subdirectories containing dictionaries.
- **Basic scaffolding**: Module structure, error types, `Translation`/`TranslationItem` structs.

## What's Completely Missing

### 1. IDX file parsing (`src/idx.rs` is an empty stub)

- No binary parsing of `word_str` (null-terminated UTF-8) + `word_data_offset` (u32 big-endian) + `word_data_size` (u32 big-endian)
- No support for 64-bit offsets (`idxoffsetbits=64` in v3.0.0)
- No binary search over the sorted word list
- No implementation of `stardict_strcmp()` (case-insensitive ASCII compare, then fallback to byte compare)
- No support for `.idx.gz` (gzip-compressed index)

### 2. DICT file reading (`src/dict.rs` is an empty stub)

- No reading of word data from offset+size
- No `sametypesequence` optimization handling (the tricky part where type chars and the last field's size/terminator are omitted)
- No parsing of the ~15 type identifiers (`m`, `t`, `h`, `g`, `x`, `W`, `P`, etc.)
- No lowercase vs uppercase type distinction (null-terminated vs length-prefixed)
- No `.dict.dz` (dictzip) decompression support

### 3. Search functions (`src/dictionary.rs:36-52`)

Return hardcoded mock data regardless of input:

```rust
fn search242(&mut self, _word: &str) -> Result<Vec<TranslationItem>> {
    // returns "hi v2.4.2" regardless of input
}
```

### 4. Synonym file (.syn)

Not mentioned anywhere in the code.

## Bugs/Issues in Existing Code

| Issue | Location | Detail |
|---|---|---|
| No magic line validation | `ifo.rs` | Spec requires first line to be exactly `"StarDict's dict ifo file"` — the parser silently skips it |
| Wrong numeric types | `ifo.rs:16-18` | `idx_file_size`, `word_count`, `syn_word_count` are `isize` (signed, platform-width) — should be `u64` or `u32` |
| No whitespace trimming | `ifo.rs:39-40` | Spec says leading/trailing blanks and blanks around `=` are discarded — not done |
| Missing `idxoffsetbits` field | `ifo.rs` | Not in the struct at all |
| Missing `dicttype` field | `ifo.rs` | Not in the struct at all |
| Assumes dir name = dict name | `dictionary.rs:19` | Uses `{dirname}.idx` — but the spec says find `.ifo` then open matching `.idx`/`.dict` with the same base name |
| No compressed file fallback | `dictionary.rs:19-21` | Never tries `.idx.gz` or `.dict.dz` |
| Deprecated dependency | `Cargo.toml` | `failure = "0.1"` abandoned since ~2019; should use `thiserror`/`anyhow` |
| Hardcoded test path | `tests/stardict.rs:8` | `/mnt/cbeta/GoldenDict/13Dicts` — test can't run anywhere else |
| Custom Error enum unused | `result.rs` | `Error` is defined but never constructed; all errors go through `failure::Error` |

## What a Production-Ready Implementation Needs

1. **IDX parser**: Read the binary index, build an in-memory sorted word list, implement binary search with `stardict_strcmp` semantics
2. **DICT reader**: Seek to offset, read `size` bytes, parse type-tagged data fields, handle `sametypesequence` optimization correctly
3. **Compression support**: `flate2` for `.idx.gz`, a dictzip crate or custom reader for `.dict.dz`
4. **Proper file discovery**: Find `.ifo`, then derive sibling filenames (not assume directory name)
5. **Synonym support**: Parse `.syn` file
6. **Modern error handling**: Replace `failure` with `thiserror`
7. **Real tests**: With embedded test dictionary fixtures, not hardcoded paths
8. **Validation**: Magic line check, version validation, `wordcount`/`idxfilesize` cross-checks

---

## Reference: JS Implementation (`../stardict.js`)

A working JavaScript StarDict reader exists at `../stardict.js` (by Thomas Vogt, MIT). It covers most of the spec and serves as a solid reference for the Rust port. Here's what it implements:

### `starDictStrCmp` (`src/modules/common.js:25-67`)
Correct spec-compliant implementation: ASCII-only case-insensitive compare first (`g_ascii_strcasecmp` semantics — only A-Z are lowered, non-ASCII left as-is), then byte-level `strcmp` tiebreaker. Operates on UTF-8 byte arrays.

### IDX binary parsing (`IdxIter` in `common.js:87-151`)
- Scans for null terminator to find `word_str` boundary
- Reads offset + size as big-endian u32 (or u64 when `idxoffsetbits=64`)
- Synonym mode: reads just a u32 `original_word_index` instead of offset+size
- Yields entries via a generator (`IdxIterator`)

### Dict entry data parsing (`_processEntryData` in `common.js:237-272`)
- Handles `sametypesequence`: strips type chars, last field consumes remaining bytes
- Without `sametypesequence`: reads type char prefix per field
- Lowercase types (`m`, `t`, `g`, `x`, `y`, `k`, `w`, `h`): null-terminated, decoded as UTF-8
- Uppercase types (`W`, `P`): length-prefixed (u32 big-endian), left as raw bytes
- Returns array of `{ type, content }` objects

### IFO parsing (`_processIfo` in `common.js:198-208`)
- Validates magic line: `"StarDict's dict ifo file"`
- Parses key=value pairs (splits on first `=`)

### Additional features
- **Synonym file (.syn)**: Fully supported — parsed via the same `IdxIterator` in "synonyms" mode
- **Dictzip (.dz)**: Supported via external `dictzip` package for random-access decompression
- **Resource storage**: Full support for `res.rifo` / `res.ridx` / `res.rdic` plus direct res files
- **Offset cache (.oft)**: Can generate or read `.idx.oft` / `.syn.oft` files
- **Three variants**: Browser async (`stardict.js`), browser sync/worker (`stardict_sync.js`), Node.js (`stardict_node.js`)

### What the JS implementation does NOT cover
- Tree dictionaries (`.tdx`)
- Collation files (`.idx.clt`)
- `dicttype=wordnet` special handling
- No binary search for word lookup (iterates the full index; search is left to the consumer)

### Test fixtures
The JS repo includes real test data at `src/tests/data/` (`testdict.idx`, `testdict.dict.dz`, `testdict.ifo`) plus inline-constructed `.syn`, `res.rifo`, `res.ridx`, `res.rdic` in the test file. These could be copied for use in Rust integration tests.

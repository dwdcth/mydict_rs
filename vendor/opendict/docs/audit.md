# Codebase Audit

## 1. .gitignore has stale names from rename

`.gitignore:26-29` still references `stardict_mobile` — generated files won't be properly ignored:

```
/expo/ios/stardict_mobile.swift
/expo/ios/stardict_mobileFFI.h
/expo/ios/stardict_mobileFFI.modulemap
/expo/ios/libstardict_mobile.a
```

Should be:

```
/expo/ios/opendict_mobile.swift
/expo/ios/opendict_mobileFFI.h
/expo/ios/opendict_mobileFFI.modulemap
/expo/ios/libopendict_mobile.a
```

## 2. MDict test coverage is almost nonexistent

### What has unit tests (4 total)

| Module | Tests | What's covered |
|--------|-------|---------------|
| `ripemd128.rs` | 3 | Known hash vectors (empty, "abc", fox sentence) |
| `decrypt.rs` | 1 | `fast_decrypt` against Python reference output |

### What has zero unit tests (8 modules)

| Module | Key functions untested |
|--------|----------------------|
| `header.rs` | `parse_header`, `parse_xml_attrs`, `decode_utf16le` |
| `keys.rs` | `parse_keywords`, `parse_key_index`, `parse_key_block` |
| `records.rs` | `parse_record_index`, `read_record` |
| `decompress.rs` | `decompress_block` (zlib, encryption, checksum verification) |
| `encoding.rs` | `decode_str`, `null_width` |
| `keygen.rs` | `derive_key` (xxhash64 v3 key derivation) |
| `file.rs` | `MdictFile::open`, `lookup_raw`, block cache logic |
| `mod.rs` | `MdictDictionary::open`, lookup, prefix search, MDD resource loading |

The mdict tests in `tests/real_dicts.rs` are all `#[ignore]` — they require real dictionary files in `tests/dicts/mdict/` and never run in CI.

### What could be tested without real dict files

- `encoding.rs` — pure functions, easy to test with synthetic byte slices
- `keygen.rs` — pure function, test against known xxhash64 outputs
- `decompress.rs` — can construct minimal zlib blocks with known checksums
- `header.rs` — `parse_xml_attrs` is pure string parsing, `decode_utf16le` is trivial
- `keys.rs` / `records.rs` — harder without real binary data, but `parse_key_block` could be tested with hand-crafted byte buffers

## 3. Dead code: `records::read_record`

`src/mdict/records.rs:49` — `pub fn read_record()` is never called anywhere. It was the original lookup path before the block cache was added to `file.rs`. The current path is `MdictFile::lookup_raw` → `get_block` (with single-entry LRU cache). The dead function does its own linear block scan without caching.

## 4. Two `DictEntry` types

There are two separate `DictEntry` structs with identical fields:

- `src/types.rs::DictEntry` — used by the `Dictionary` trait
- `src/stardict/dict.rs::DictEntry` — used internally by StarDict's `Dict::read_entry`

The trait impl in `stardict/mod.rs` (lines 134-141, 160-167) manually maps between them field-by-field. The legacy `dictionary` module also re-exports the StarDict-internal one via `pub use super::stardict::dict::DictEntry`.

These could be unified into a single type.

## 5. `unwrap()` calls that can panic on malformed input

### `src/stardict/dict.rs:119,142`

```rust
let null_pos = raw.iter().position(|&b| b == 0).unwrap();
```

If a non-last `sametypesequence` field has no null terminator (malformed dict data), this panics instead of returning an error. Should use `ok_or` / `anyhow!`.

### `src/stardict/mod.rs:25`

```rust
let name = ifo_path.file_stem().unwrap().to_str().unwrap().to_string();
```

Panics on filenames that are non-UTF8 or have no stem. Should propagate an error.

## 6. Duplicated logic in `stardict/mod.rs`

### `open_dir` vs `open`

Lines 23-53 (`open_dir`) and lines 56-83 (`open`) share nearly identical code for loading idx/dict/syn files. The only difference is how the ifo path and name are obtained. Could share a common `open_from_ifo` helper.

### Inherent impl vs trait impl

`lookup` and `lookup_synonym` are implemented twice — once on the struct directly (returns `Vec<dict::DictEntry>`) and once for the `Dictionary` trait (returns `Vec<types::DictEntry>`). The trait impl is just a wrapper that maps between the two DictEntry types. This would go away if the two DictEntry types were unified (#4).

## 7. Dict.gz decompresses to disk as a side effect

`src/stardict/dict.rs:34-43` — When opening a `.dict.dz` (gzip-compressed) file, the code silently writes a decompressed `.dict` file next to it on disk, then mmaps that. This is a side effect that could surprise users:

- Writes to the dictionary directory without being asked
- On a read-only filesystem it falls back to in-memory, but still attempts the write first
- The `.gitignore` has `*.dict` / `!tests/fixtures/*.dict` to hide these from git

## 8. MDict reads entire file into memory

`src/mdict/file.rs:25` does `std::fs::read(path)?` for both `.mdx` and `.mdd` files, loading everything into a `Vec<u8>`. For large dictionaries (Oxford OED's .mdx is 100MB+, plus companion .mdd files for resources), this means significant memory usage.

By contrast, StarDict uses `mmap` for its `.dict` data (`src/stardict/dict.rs:56`), letting the OS page data in and out as needed.

This could be addressed by mmapping the mdx/mdd files and adjusting the block decompression to work with `&[u8]` slices from the mmap.

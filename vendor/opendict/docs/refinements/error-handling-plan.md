# Error Handling Plan

## Error Type

Three-variant enum covering the three things callers actually need to react to:

```rust
#[derive(Debug)]
pub enum Error {
    /// File I/O failures (not found, permission denied, read errors)
    Io(std::io::Error),

    /// File is corrupt or malformed (bad magic, truncated, checksum mismatch)
    InvalidFormat(String),

    /// File uses features not yet implemented (LZO, Salsa20, v1.2, etc.)
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

Implement `std::error::Error`, `Display`, and `From<std::io::Error>`. Lives in `src/error.rs`, re-exported from `src/lib.rs`.

## Dictionary Trait

```rust
pub trait Dictionary {
    fn lookup(&self, word: &str) -> Result<Option<Vec<DictEntry>>>;
    fn lookup_synonym(&self, word: &str) -> Result<Option<Vec<DictEntry>>>;
    fn word_list(&self) -> Vec<String>;
    fn word_count(&self) -> usize;
    fn info(&self) -> &DictInfo;
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;
}
```

- `Ok(Some(entries))` — found the word
- `Ok(None)` — word genuinely not in the dictionary
- `Err(e)` — something went wrong reading record data

`word_count`, `info`, `search_prefix`, `word_list` stay infallible — they operate on in-memory data validated at open time.

## Changes By File

### src/error.rs (new)

Create the `Error` enum, `Result` type alias, trait impls.

### src/lib.rs

- Replace `anyhow::Result` with `crate::Result` in `open()`.
- Remove all legacy re-exports (`pub use stardict::ifo`, etc.).
- Remove the `dictionary` module alias.
- Re-export `Error`, `Result` from the crate root.

### src/stardict/mod.rs

- `StarDictDictionary::open_dir()` — change `anyhow::Result` to `crate::Result`. Map `anyhow!("no .ifo file")` to `Error::InvalidFormat`.
- `StarDictDictionary::open()` — same.
- `StarDictDictionary::open_from_ifo()` — same.
- `lookup()` — change return to `Result<Option<Vec<DictEntry>>>`. Propagate errors from `dict.read_entry()`.
- `lookup_synonym()` — same.
- `Dictionary` trait impl — update signatures, delegate to inherent methods.
- Deduplicate `word_list()` — trait impl should call `self.word_list()`.

### src/stardict/dict.rs

- `Dict::open()` — change `anyhow::Result` to `crate::Result`. I/O errors become `Error::Io`, format errors become `Error::InvalidFormat`.
- `read_entry()` — **change return to `Result<Vec<DictEntry>>`**. Add bounds check on `start..end` before slicing. Return `Error::InvalidFormat` if offset/size exceeds data.
- `parse_entries()` — **change return to `Result<Vec<DictEntry>>`**. Replace `None => break` with `Error::InvalidFormat("missing null terminator")`.

### src/stardict/idx.rs

- `Idx::open()` — change `anyhow::Result` to `crate::Result`.
- `word_at()` — **make `pub(crate)`**. Keep the unwrap (validated at open time).
- `entry()` — **make `pub(crate)`**. Keep the unwrap.
- `search()` — keep returning `Option` (not fallible, operates on in-memory data).
- `search_prefix()` — keep returning `Vec<String>`.
- Remove unused `_word_count` parameter from `open()`.

### src/stardict/ifo.rs

- `Ifo::open()` — change `anyhow::Result` to `crate::Result`. Map missing fields to `Error::InvalidFormat`, unsupported versions to `Error::Unsupported`.

### src/stardict/syn.rs

- `Syn::open()` — change `anyhow::Result` to `crate::Result`.
- `word_at()` — **make `pub(crate)`**. Keep the unwrap.
- `entry()` — **make `pub(crate)`**. Keep the unwrap.
- `lookup()` — keep returning `Option`.

### src/stardict/io.rs

- `read_file()` — change `anyhow::Result` to `crate::Result`.

### src/mdict/mod.rs

- `MdictDictionary::open()` — change `anyhow::Result` to `crate::Result`.
- `Dictionary::lookup()` — change return to `Result<Option<Vec<DictEntry>>>`. Propagate errors from `mdx.lookup_raw()`.
- `find_mdx()` — change `anyhow::Result` to `crate::Result`.
- `load_mdd_files()` — **remove `eprintln!`**. Use `log::warn!` instead. Keep returning `Vec` (MDD failures are non-fatal).

### src/mdict/file.rs

- `MdictFile::open()` — change `anyhow::Result` to `crate::Result`.
- `get_block()` — change `anyhow::Result` to `crate::Result`. Replace `.unwrap()` on mutex lock with `.lock().unwrap_or_else(|e| e.into_inner())` (recover from poison rather than panic).
- `lookup_raw()` — **change return to `Result<Option<Vec<u8>>>`**. Replace `.ok()?` with `?` to propagate decompression errors instead of swallowing them.

### src/mdict/header.rs

- `parse_header()` — change `anyhow::Result` to `crate::Result`.
- **Remove `unwrap_or(2.0)` for version** — if the version string is garbage, return `Error::InvalidFormat`.
- **Remove `unwrap_or(0)` for encrypted field** — same, return `Error::InvalidFormat`.
- `decode_utf16le()` — change `anyhow::Result` to `crate::Result`.

### src/mdict/keys.rs

- `parse_keywords()` — change `anyhow::Result` to `crate::Result`. Map encrypted keyword header to `Error::Unsupported`.
- `parse_key_index()` — change `anyhow::Result` to `crate::Result`.
- `parse_key_block()` — change `anyhow::Result` to `crate::Result`. **Replace `break` on truncated data with `Error::InvalidFormat`**.

### src/mdict/records.rs

- `parse_record_index()` — change `anyhow::Result` to `crate::Result`.

### src/mdict/decompress.rs

- `decompress_block()` — change `anyhow::Result` to `crate::Result`. Map LZO/Salsa20 to `Error::Unsupported`, checksum mismatches to `Error::InvalidFormat`.

### src/mdict/encoding.rs

- **Keep lossy decoding** — this is correct for a dictionary reader. Add a doc comment explaining the intentional choice:
  ```rust
  /// Decodes bytes from the source encoding to a UTF-8 String.
  /// Uses lossy conversion: invalid sequences become U+FFFD replacement
  /// characters. This is intentional — showing a definition with a few
  /// bad characters is better than failing the entire lookup.
  ```

### src/mdict/keygen.rs

- No changes needed — returns `Option`, which is correct.

### src/mdict/ripemd128.rs, decrypt.rs

- No changes needed — pure functions, no error handling.

## Dependencies

- **Remove `anyhow`** from `[dependencies]`. No longer needed once all callsites use `crate::Error`.
- **Add `log`** to `[dependencies]` for MDD load warnings.
- Keep `anyhow` in `[dev-dependencies]` if any tests/examples use it.

## Test Changes

- All integration tests that access internal types (`tests/dict.rs`, `tests/idx.rs`, `tests/ifo.rs`, `tests/syn.rs`, `tests/strcmp.rs`) need to be moved into `#[cfg(test)]` modules inside their source files, since the re-exports they depend on are being removed.
- `tests/integration.rs`, `tests/mdict_fixture.rs`, `tests/real_dicts.rs` stay as integration tests — update them for the new `Result<Option<_>>` return types.
- Remove `extern crate opendict;` from all test files.

## Caller-Facing Example

```rust
// Loading dictionaries
for dir in dict_dirs {
    match opendict::open(&dir) {
        Ok(dict) => dicts.push(dict),
        Err(opendict::Error::Unsupported(msg)) => {
            log::info!("Skipping {}: {}", dir.display(), msg);
        }
        Err(opendict::Error::Io(e)) if e.kind() == ErrorKind::NotFound => {
            // not a dictionary directory, skip
        }
        Err(e) => {
            log::error!("Failed to load {}: {}", dir.display(), e);
        }
    }
}

// Looking up a word
match dict.lookup("hello") {
    Ok(Some(entries)) => show(entries),
    Ok(None) => println!("Not found"),
    Err(e) => eprintln!("Error reading dictionary: {}", e),
}
```

# Remaining Implementation

## Overview

Three things left:

1. **Compressed file support** (`.dict.dz`, `.idx.gz`) — new dep `flate2`, touch `idx.rs`, `dict.rs`, `dictionary.rs`
2. **Fix legacy `search()` method** — remove hardcoded mocks in `dictionary.rs`
3. **Resource storage** (`.rifo`/`.ridx`/`.rdic`) — entirely new module, low priority

---

## 1. Compressed file support

### What the spec says

- `.idx.gz` — standard gzip. "You can use gzip -9 to compress the .idx file."
- `.dict.dz` — dictzip format. Same compression as gzip, gunzip-compatible, but adds a random-access table in the gzip header. Since we load the entire file into memory (`Dict.data`), we don't need random access — just decompress it as regular gzip.

### Dependency

```toml
# Cargo.toml
[dependencies]
anyhow = "1"
flate2 = "1"
```

### `src/idx.rs`

`Idx::open` currently calls `fs::read(&file)`. Change it to try the path as-is, then fall back to `.gz` variant, decompressing if needed. Actually, `dictionary.rs` already constructs the path — better to add a helper that reads a file with optional gzip decompression, and have `dictionary.rs` pass the right path.

Add a shared helper in a new `src/io.rs`:

```rust
// src/io.rs
use std::fs;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;

/// Read a file, decompressing with gzip if the path ends in .gz or .dz.
pub fn read_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let data = fs::read(path)?;
    if path.extension().map_or(false, |ext| ext == "gz" || ext == "dz") {
        let mut decoder = GzDecoder::new(&data[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        Ok(decompressed)
    } else {
        Ok(data)
    }
}
```

### `src/idx.rs` change

```rust
// BEFORE
let data = fs::read(&file)?;

// AFTER
let data = crate::io::read_file(&file)?;
```

Remove `use std::fs;`.

### `src/dict.rs` change

```rust
// BEFORE (in Dict::open)
let data = fs::read(&file)?;

// AFTER
let data = crate::io::read_file(&file)?;
```

Remove `use std::fs;`.

### `src/dictionary.rs` — file discovery

The spec says: "Found an .ifo file, StarDict opens the .idx or .idx.gz file and the .dict.dz or .dict file which is in the same directory and has the same base name."

Update `Dictionary::open` to try compressed variants:

```rust
pub fn open(dir: path::PathBuf, name: &str) -> anyhow::Result<Dictionary> {
    let ifo = Ifo::open(dir.join(format!("{}.ifo", name)))?;

    // Try .idx first, then .idx.gz
    let idx_path = dir.join(format!("{}.idx", name));
    let idx_path = if idx_path.exists() {
        idx_path
    } else {
        dir.join(format!("{}.idx.gz", name))
    };
    let idx = Idx::open(idx_path, ifo.word_count as u32, ifo.idx_offset_bits)?;

    // Try .dict first, then .dict.dz
    let dict_path = dir.join(format!("{}.dict", name));
    let dict_path = if dict_path.exists() {
        dict_path
    } else {
        dir.join(format!("{}.dict.dz", name))
    };
    let dict = Dict::open(dict_path)?;

    let syn_path = dir.join(format!("{}.syn", name));
    let syn = if syn_path.exists() && ifo.syn_word_count > 0 {
        Some(Syn::open(syn_path, ifo.syn_word_count as u32)?)
    } else {
        None
    };

    Ok(Dictionary { idx, ifo, dict, syn })
}
```

### `src/lib.rs`

Add `pub mod io;` to the module list.

---

## 2. Fix legacy `search()` method

`Dictionary::search()` dispatches to `search242`/`search300` which return hardcoded mock data. Replace with a single implementation that delegates to `lookup()`:

```rust
// BEFORE
pub fn search(&mut self, word: &str) -> anyhow::Result<Vec<TranslationItem>> {
    match self.ifo.version.as_ref() {
        "2.4.2" => self.search242(word),
        "3.0.0" => self.search300(word),
        v => Err(anyhow!("bad dictionary version {}", v)),
    }
}

fn search242(&mut self, _word: &str) -> anyhow::Result<Vec<TranslationItem>> {
    let mut items = Vec::new();
    items.push(TranslationItem {
        mode: 'h',
        body: "hi v2.4.2".to_string(),
    });
    Ok(items)
}

fn search300(&mut self, _word: &str) -> anyhow::Result<Vec<TranslationItem>> {
    let mut items = Vec::new();
    items.push(TranslationItem {
        mode: 'h',
        body: "hi v3.0.0".to_string(),
    });
    Ok(items)
}

// AFTER
pub fn search(&self, word: &str) -> anyhow::Result<Vec<TranslationItem>> {
    match self.lookup(word) {
        Some(entries) => Ok(entries
            .into_iter()
            .map(|e| TranslationItem {
                mode: e.type_id,
                body: String::from_utf8_lossy(&e.data).into_owned(),
            })
            .collect()),
        None => Ok(Vec::new()),
    }
}
```

Delete `search242`, `search300`. Also change `&mut self` to `&self` since lookup is immutable.

This means `StarDict::search` in `lib.rs` also drops `&mut`:

```rust
// BEFORE
pub fn search(&mut self, word: &str) -> Vec<dictionary::Translation> {
    ...
    for it in &mut self.directories {

// AFTER
pub fn search(&self, word: &str) -> Vec<dictionary::Translation> {
    ...
    for it in &self.directories {
```

Same for `info()`:

```rust
// BEFORE
pub fn info(&mut self) -> Vec<ifo::Ifo> {
    for it in &mut self.directories {

// AFTER
pub fn info(&self) -> Vec<ifo::Ifo> {
    for it in &self.directories {
```

---

## 3. Resource storage (low priority)

The spec describes `.rifo`/`.ridx`/`.rdic` files for external resources (images, sounds, etc). Format is similar to the main dict:

- `res.rifo` — same IFO format but magic `"StarDict's storage ifo file"`, fields: `filecount`, `ridxfilesize`, `idxoffsetbits`
- `res.ridx` — same binary format as `.idx`: `filename\0` + offset + size, sorted by `strcmp()` (not `stardict_strcmp`)
- `res.rdic` — concatenated resource files, can be dictzip'd as `res.rdic.dz`

This is a separate feature and not needed for basic dictionary reading. Skip for now.

---

## Files changed

| File | Change |
|---|---|
| `Cargo.toml` | Add `flate2 = "1"` |
| `src/io.rs` | **New** — `read_file()` helper |
| `src/lib.rs` | Add `pub mod io;`, drop `&mut` from `search`/`info` |
| `src/idx.rs` | Use `crate::io::read_file` instead of `fs::read` |
| `src/dict.rs` | Use `crate::io::read_file` instead of `fs::read` |
| `src/dictionary.rs` | File discovery fallback (.gz/.dz), replace `search` method |

## Verification

```sh
cargo test                    # existing 87 tests still pass
cargo test -- --ignored       # real dict tests (many real dicts use .dict.dz)
```

Could also add a test that loads `testdict.dict.dz` directly (already in fixtures).

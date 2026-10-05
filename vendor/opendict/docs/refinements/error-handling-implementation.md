# Error Handling — Exact Code Changes

Replace `anyhow` in library dependencies with a three-variant `Error` enum (`Io`, `InvalidFormat`, `Unsupported`). Changes `lookup`/`lookup_synonym` from `Option<Vec<DictEntry>>` to `Result<Option<Vec<DictEntry>>>` so I/O and format errors propagate instead of being swallowed as "not found". Changes `Dictionary::info()` to return `&DictInfo` instead of cloning.

File change order is bottom-up from leaf modules to root, to avoid intermediate compilation errors.

---

## 1. NEW: `src/error.rs`

```rust
use std::fmt;

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

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::InvalidFormat(msg) => write!(f, "invalid format: {}", msg),
            Error::Unsupported(msg) => write!(f, "unsupported: {}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
```

---

## 2. `Cargo.toml`

- Remove `anyhow = "1"` from `[dependencies]` entirely (nothing uses it)
- Add `log = "0.4"` to `[dependencies]`

---

## 3. `src/lib.rs`

Full replacement:

```rust
pub mod error;
pub mod types;
pub mod stardict;
pub mod mdict;

use std::path;

pub use error::{Error, Result};
pub use types::{DictEntry, DictInfo};

pub trait Dictionary {
    fn lookup(&self, word: &str) -> Result<Option<Vec<DictEntry>>>;
    fn lookup_synonym(&self, word: &str) -> Result<Option<Vec<DictEntry>>>;
    fn word_list(&self) -> Vec<String>;
    fn word_count(&self) -> usize;
    fn info(&self) -> &DictInfo;
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;
}

pub fn open(dir: impl AsRef<path::Path>) -> Result<Box<dyn Dictionary + Send + Sync>> {
    let dir = dir.as_ref();
    match stardict::StarDictDictionary::open_dir(dir.to_path_buf()) {
        Ok(dict) => return Ok(Box::new(dict)),
        Err(_) => {}
    }
    let dict = mdict::MdictDictionary::open(dir.to_path_buf())?;
    Ok(Box::new(dict))
}
```

Changes: add `pub mod error`, re-export `Error`/`Result`, `lookup`/`lookup_synonym` return `Result<Option<_>>`, `info()` returns `&DictInfo`, `open()` returns `Result` not `anyhow::Result`.

---

## 4. `src/stardict/io.rs`

Signature change only. Remove `anyhow` import, body unchanged — `io::Error` converts via `From`.

```rust
use std::fs;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;

/// Read a file, decompressing with gzip if the path ends in .gz or .dz.
pub fn read_file(path: &Path) -> crate::Result<Vec<u8>> {
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

---

## 5. `src/stardict/ifo.rs`

Replace `use anyhow::anyhow` with `use crate::error::Error`. Change return type, map each error:

```rust
use std::io::BufRead;
use std::{fs, io, path};

use crate::error::Error;

// ... Ifo struct unchanged ...

impl Ifo {
    pub fn open(file: path::PathBuf) -> crate::Result<Ifo> {
        // ... fields init unchanged ...

        for line in io::BufReader::new(fs::File::open(file)?).lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if !magic_checked {
                if trimmed != "StarDict's dict ifo file" {
                    return Err(Error::InvalidFormat(format!(
                        "invalid magic line: {}", trimmed
                    )));
                }
                magic_checked = true;
                continue;
            }

            if let Some(id) = trimmed.find('=') {
                let key = trimmed[..id].trim();
                let val = trimmed[id + 1..].trim().to_string();
                match key {
                    "author" => it.author = val,
                    "bookname" => {
                        it.name = val;
                        has_name = true;
                    }
                    "version" => {
                        match val.as_str() {
                            "2.4.2" | "3.0.0" => it.version = val,
                            v => return Err(Error::Unsupported(format!(
                                "unsupported ifo version: {}", v
                            ))),
                        }
                    }
                    "description" => it.description = val,
                    "date" => it.date = val,
                    "idxfilesize" => {
                        it.idx_file_size = val.parse().map_err(|e| {
                            Error::InvalidFormat(format!("invalid idxfilesize: {}", e))
                        })?;
                        has_idx_file_size = true;
                    }
                    "wordcount" => {
                        it.word_count = val.parse().map_err(|e| {
                            Error::InvalidFormat(format!("invalid wordcount: {}", e))
                        })?;
                        has_word_count = true;
                    }
                    "website" => it.web_site = val,
                    "email" => it.email = val,
                    "sametypesequence" => it.same_type_sequence = val,
                    "synwordcount" => {
                        it.syn_word_count = val.parse().map_err(|e| {
                            Error::InvalidFormat(format!("invalid synwordcount: {}", e))
                        })?;
                    }
                    "idxoffsetbits" => {
                        it.idx_offset_bits = val.parse().map_err(|e| {
                            Error::InvalidFormat(format!("invalid idxoffsetbits: {}", e))
                        })?;
                    }
                    _ => {}
                };
            }
        }

        if !has_name {
            return Err(Error::InvalidFormat(
                "missing required field: bookname".into(),
            ));
        }
        if !has_word_count {
            return Err(Error::InvalidFormat(
                "missing required field: wordcount".into(),
            ));
        }
        if !has_idx_file_size {
            return Err(Error::InvalidFormat(
                "missing required field: idxfilesize".into(),
            ));
        }

        Ok(it)
    }
}
```

Tests unchanged — they call `Ifo::open(...)` and check `is_err()`/`is_ok()`, which works regardless of error type.

---

## 6. `src/stardict/idx.rs`

Replace `use anyhow::anyhow` with `use crate::error::Error`. Remove `_word_count` parameter. Change visibility on `word_at`/`entry`.

```rust
use std::{path, str};

use crate::error::Error;

use super::strcmp::stardict_strcmp;

// ... IdxEntry, Idx structs unchanged ...

impl Idx {
    pub fn open(file: path::PathBuf, offset_bits: u32) -> crate::Result<Idx> {
        let data = super::io::read_file(&file)?;
        let ref_size: usize = if offset_bits == 64 { 12 } else { 8 };

        let mut offsets = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            offsets.push(pos as u32);
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| Error::InvalidFormat(format!(
                    "idx: missing null terminator at offset {}", pos
                )))?;
            str::from_utf8(&data[pos..pos + null_pos]).map_err(|e| {
                Error::InvalidFormat(format!("idx: invalid UTF-8 at offset {}: {}", pos, e))
            })?;
            let ref_start = pos + null_pos + 1;
            if ref_start + ref_size > data.len() {
                return Err(Error::InvalidFormat(format!(
                    "idx: unexpected EOF at offset {}", ref_start
                )));
            }
            pos = ref_start + ref_size;
        }

        Ok(Idx { data, offsets, offset_bits })
    }

    pub fn entry_count(&self) -> usize {
        self.offsets.len()
    }

    /// Word for entry i -- zero-copy from raw buffer.
    pub(crate) fn word_at(&self, i: usize) -> &str {
        // body unchanged
    }

    /// Construct a full IdxEntry for entry i (allocates a String).
    pub(crate) fn entry(&self, i: usize) -> IdxEntry {
        // body unchanged
    }

    // search, binary_search, search_prefix — unchanged
    // data_len, offsets_len — unchanged
}
```

Update tests — remove the second (word count) arg from every `Idx::open` call:

```rust
// Before:
let idx = Idx::open(fixture("testdict.idx"), 4, 32).unwrap();
// After:
let idx = Idx::open(fixture("testdict.idx"), 32).unwrap();

// Before:
let idx = Idx::open(path, 2, 64).unwrap();
// After:
let idx = Idx::open(path, 64).unwrap();

// Before:
let idx = Idx::open(path, 2, 32).unwrap();
// After:
let idx = Idx::open(path, 32).unwrap();
```

---

## 7. `src/stardict/syn.rs`

Replace `use anyhow::anyhow` with `use crate::error::Error`. Change visibility on `word_at`/`entry`.

```rust
use std::{fs, path, str};

use crate::error::Error;

// ... SynEntry, Syn structs unchanged ...

impl Syn {
    pub fn open(file: path::PathBuf, _syn_word_count: u32) -> crate::Result<Syn> {
        let data = fs::read(&file)?;
        let mut offsets = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            offsets.push(pos as u32);
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| Error::InvalidFormat(format!(
                    "syn: missing null terminator at offset {}", pos
                )))?;
            str::from_utf8(&data[pos..pos + null_pos]).map_err(|e| {
                Error::InvalidFormat(format!("syn: invalid UTF-8 at offset {}: {}", pos, e))
            })?;
            let idx_start = pos + null_pos + 1;
            if idx_start + 4 > data.len() {
                return Err(Error::InvalidFormat(
                    "syn: unexpected EOF reading word index".into(),
                ));
            }
            pos = idx_start + 4;
        }

        Ok(Syn { data, offsets })
    }

    pub fn entry_count(&self) -> usize {
        self.offsets.len()
    }

    pub(crate) fn word_at(&self, i: usize) -> &str {
        // body unchanged
    }

    pub(crate) fn entry(&self, i: usize) -> SynEntry {
        // body unchanged
    }

    // lookup, data_len, offsets_len — unchanged
}
```

Tests unchanged — they don't call `word_at`/`entry` from outside the module.

---

## 8. `src/stardict/dict.rs`

Add bounds check to `read_entry`, change `parse_entries` to return errors on missing null terminators.

```rust
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path;

use flate2::read::GzDecoder;
use memmap2::Mmap;

use crate::error::Error;
use crate::types::DictEntry;

// ... DictData enum unchanged ...
// ... Dict struct unchanged ...

impl Dict {
    pub fn open(file: path::PathBuf, cache_to_disk: bool) -> crate::Result<Dict> {
        // body unchanged — io::Error converts via From
    }

    fn open_mmap(file: &path::Path) -> crate::Result<Dict> {
        // body unchanged
    }

    // bytes(), data_len(), storage_label() — unchanged

    pub fn read_entry(
        &self,
        offset: u64,
        size: u32,
        sametypesequence: Option<&str>,
    ) -> crate::Result<Vec<DictEntry>> {
        let data = self.bytes();
        let start = offset as usize;
        let end = start + size as usize;
        if end > data.len() {
            return Err(Error::InvalidFormat(format!(
                "dict entry offset {}+{} exceeds data length {}",
                start, size, data.len()
            )));
        }
        let raw = &data[start..end];
        Self::parse_entries(raw, sametypesequence)
    }

    fn parse_entries(data: &[u8], sametypesequence: Option<&str>) -> crate::Result<Vec<DictEntry>> {
        let mut raw = data;
        let mut entries = Vec::new();

        match sametypesequence {
            Some(sts) => {
                let types: Vec<char> = sts.chars().collect();
                for (i, &type_id) in types.iter().enumerate() {
                    let is_last = i == types.len() - 1;
                    if is_last {
                        entries.push(DictEntry {
                            type_id,
                            data: raw.to_vec(),
                        });
                        break;
                    }
                    if type_id.is_ascii_uppercase() {
                        let len =
                            u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).ok_or_else(|| {
                            Error::InvalidFormat(
                                "missing null terminator in dict entry".into(),
                            )
                        })?;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[..null_pos].to_vec(),
                        });
                        raw = &raw[null_pos + 1..];
                    }
                }
            }
            None => {
                while !raw.is_empty() {
                    let type_id = raw[0] as char;
                    raw = &raw[1..];

                    if type_id.is_ascii_uppercase() {
                        let len =
                            u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).ok_or_else(|| {
                            Error::InvalidFormat(
                                "missing null terminator in dict entry".into(),
                            )
                        })?;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[..null_pos].to_vec(),
                        });
                        raw = &raw[null_pos + 1..];
                    }
                }
            }
        }

        Ok(entries)
    }
}
```

Update unit tests — `read_entry` now returns `Result`, so existing calls need no change (they don't unwrap the return, they just use the `Vec` directly). Wait — actually they do use the return directly without unwrap:

```rust
// Before:
let entries = dict.read_entry(0, 8, Some("m"));
// After:
let entries = dict.read_entry(0, 8, Some("m")).unwrap();
```

Every `dict.read_entry(...)` call in the `#[cfg(test)]` block needs `.unwrap()` appended. Same for `Dict::parse_entries(...)` if called directly. And `Self::parse_entries` inside `read_entry` uses `?` so that's fine.

---

## 9. `src/stardict/mod.rs`

Full replacement:

```rust
pub mod ifo;
pub mod idx;
pub mod dict;
pub mod syn;
pub mod strcmp;
pub mod io;

use std::path;

use crate::error::Error;
use crate::types::{DictEntry, DictInfo};
use crate::Dictionary;

pub struct StarDictDictionary {
    pub(crate) idx: idx::Idx,
    pub(crate) ifo: ifo::Ifo,
    pub(crate) dict: dict::Dict,
    pub(crate) syn: Option<syn::Syn>,
    info_data: DictInfo,
}

impl StarDictDictionary {
    /// Auto-detect: finds the .ifo file in the directory.
    pub fn open_dir(dir: path::PathBuf) -> crate::Result<Self> {
        let ifo_path = find_file_with_ext(&dir, "ifo")?;
        let name = ifo_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::InvalidFormat(format!(
                "invalid .ifo filename: {}", ifo_path.display()
            )))?
            .to_string();
        let ifo = ifo::Ifo::open(ifo_path)?;
        Self::open_from_ifo(&dir, &name, ifo)
    }

    /// Load a dictionary from a directory, given the dictionary name prefix.
    pub fn open(dir: path::PathBuf, name: &str) -> crate::Result<Self> {
        let ifo = ifo::Ifo::open(dir.join(format!("{}.ifo", name)))?;
        Self::open_from_ifo(&dir, name, ifo)
    }

    fn open_from_ifo(dir: &path::Path, name: &str, ifo: ifo::Ifo) -> crate::Result<Self> {
        let idx_path = dir.join(format!("{}.idx", name));
        let idx_path = if idx_path.exists() {
            idx_path
        } else {
            dir.join(format!("{}.idx.gz", name))
        };
        let idx = idx::Idx::open(idx_path, ifo.idx_offset_bits)?;

        let dict_path = dir.join(format!("{}.dict", name));
        let dict_path = if dict_path.exists() {
            dict_path
        } else {
            dir.join(format!("{}.dict.dz", name))
        };
        let dict = dict::Dict::open(dict_path, true)?;

        let syn_path = dir.join(format!("{}.syn", name));
        let syn = if syn_path.exists() && ifo.syn_word_count > 0 {
            Some(syn::Syn::open(syn_path, ifo.syn_word_count as u32)?)
        } else {
            None
        };

        let info_data = DictInfo {
            name: ifo.name.clone(),
            author: ifo.author.clone(),
            description: ifo.description.clone(),
            word_count: ifo.word_count,
        };

        Ok(StarDictDictionary { idx, ifo, dict, syn, info_data })
    }

    /// Return a reference to the IFO metadata.
    pub fn ifo(&self) -> &ifo::Ifo {
        &self.ifo
    }

    /// Look up a word and return its dictionary entries.
    pub fn lookup(&self, word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        let entry = match self.idx.search(word) {
            Some(e) => e,
            None => return Ok(None),
        };
        let sametypesequence = if self.ifo.same_type_sequence.is_empty() {
            None
        } else {
            Some(self.ifo.same_type_sequence.as_str())
        };
        self.dict.read_entry(entry.offset, entry.size, sametypesequence).map(Some)
    }

    /// Look up a synonym and return the dictionary entries for its target word.
    pub fn lookup_synonym(&self, synonym: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        let syn = match self.syn.as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syn_entry = match syn.lookup(synonym) {
            Some(e) => e,
            None => return Ok(None),
        };
        let i = syn_entry.original_word_index as usize;
        if i >= self.idx.entry_count() {
            return Ok(None);
        }
        let target = self.idx.entry(i);
        let sametypesequence = if self.ifo.same_type_sequence.is_empty() {
            None
        } else {
            Some(self.ifo.same_type_sequence.as_str())
        };
        self.dict.read_entry(target.offset, target.size, sametypesequence).map(Some)
    }

    /// Return a list of all words in the index.
    pub fn word_list(&self) -> Vec<String> {
        (0..self.idx.entry_count())
            .map(|i| self.idx.word_at(i).to_string())
            .collect()
    }
}

impl Dictionary for StarDictDictionary {
    fn lookup(&self, word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        self.lookup(word)
    }

    fn lookup_synonym(&self, word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        self.lookup_synonym(word)
    }

    fn word_list(&self) -> Vec<String> {
        self.word_list()
    }

    fn word_count(&self) -> usize {
        self.idx.entry_count()
    }

    fn info(&self) -> &DictInfo {
        &self.info_data
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.idx.search_prefix(prefix, limit)
    }
}

fn find_file_with_ext(dir: &path::Path, ext: &str) -> crate::Result<path::PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().map_or(false, |e| e == ext) {
            return Ok(path);
        }
    }
    Err(Error::InvalidFormat(format!(
        "no .{} file found in {}", ext, dir.display()
    )))
}
```

Changes vs current:
- Remove `use anyhow::anyhow`
- Add `info_data: DictInfo` field to struct
- All `open*` methods: `anyhow::Result` → `crate::Result`
- `open_from_ifo`: drop `_word_count` arg from `Idx::open`, build `info_data`
- Rename inherent `info()` → `ifo()`
- `lookup`/`lookup_synonym`: `Option<Vec<DictEntry>>` → `crate::Result<Option<Vec<DictEntry>>>`
- `Some(self.dict.read_entry(...))` → `self.dict.read_entry(...).map(Some)`
- Trait impl: updated signatures, `info()` returns `&self.info_data`
- `find_file_with_ext`: `anyhow!` → `Error::InvalidFormat`

---

## 10. `src/mdict/decompress.rs`

```rust
use crate::error::Error;
use flate2::read::ZlibDecoder;
use std::io::Read;

pub fn decompress_block(
    block: &[u8],
    version: f32,
    global_key: Option<&[u8; 16]>,
) -> crate::Result<Vec<u8>> {
    if block.len() < 8 {
        return Err(Error::InvalidFormat("block too small".into()));
    }

    let info = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    let compression_method = info & 0xf;
    let encryption_method = (info >> 4) & 0xf;
    let encryption_size = ((info >> 8) & 0xff) as usize;

    let stored_checksum = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
    let mut data = block[8..].to_vec();

    if encryption_method == 1 {
        let key = match global_key {
            Some(k) => *k,
            None => super::ripemd128::ripemd128(&block[4..8]),
        };
        let n = if encryption_size > 0 {
            encryption_size.min(data.len())
        } else {
            data.len()
        };
        let decrypted = super::decrypt::fast_decrypt(&data[..n], &key);
        data[..n].copy_from_slice(&decrypted);
    } else if encryption_method != 0 {
        return Err(Error::Unsupported(format!(
            "unsupported per-block encryption method: {}",
            encryption_method
        )));
    }

    if version >= 3.0 {
        let computed = adler2::adler32_slice(&data);
        if computed != stored_checksum {
            return Err(Error::InvalidFormat(format!(
                "adler32 mismatch (v3 decrypted): stored={:#010x} computed={:#010x}",
                stored_checksum, computed
            )));
        }
    }

    let decompressed = match compression_method {
        0 => data,
        1 => return Err(Error::Unsupported(
            "LZO compression not yet supported".into(),
        )),
        2 => {
            let mut decoder = ZlibDecoder::new(&data[..]);
            let mut buf = Vec::new();
            decoder.read_to_end(&mut buf)?;
            buf
        }
        _ => return Err(Error::Unsupported(format!(
            "unknown compression type: {}", compression_method
        ))),
    };

    if version < 3.0 {
        let computed = adler2::adler32_slice(&decompressed);
        if computed != stored_checksum {
            return Err(Error::InvalidFormat(format!(
                "adler32 mismatch (v2 decompressed): stored={:#010x} computed={:#010x}",
                stored_checksum, computed
            )));
        }
    }

    Ok(decompressed)
}
```

---

## 11. `src/mdict/header.rs`

Replace `use anyhow::anyhow` with `use crate::error::Error`. Remove `unwrap_or` fallbacks on version and encrypted fields:

```rust
use crate::error::Error;

// ... MdictHeader struct unchanged ...

pub fn parse_header(data: &[u8]) -> crate::Result<MdictHeader> {
    if data.len() < 8 {
        return Err(Error::InvalidFormat("file too small".into()));
    }

    let header_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

    if data.len() < 4 + header_len + 4 {
        return Err(Error::InvalidFormat("file truncated in header".into()));
    }

    let header_bytes = &data[4..4 + header_len];
    let header_str = decode_utf16le(header_bytes)?;

    let keyword_sect_start = 4 + header_len + 4;

    let mut version = 2.0f32;
    let mut encoding = "UTF-8".to_string();
    let mut format = "Html".to_string();
    let mut title = String::new();
    let mut description = String::new();
    let mut encrypted = 0u8;
    let mut key_case_sensitive = false;
    let mut uuid: Option<Vec<u8>> = None;

    for (key, val) in parse_xml_attrs(&header_str) {
        match key.as_str() {
            "GeneratedByEngineVersion" => {
                version = val.parse().map_err(|e| {
                    Error::InvalidFormat(format!(
                        "invalid engine version '{}': {}", val, e
                    ))
                })?;
            }
            "Encoding" => encoding = val,
            "Format" => format = val,
            "Title" => title = val,
            "Description" => description = val,
            "Encrypted" => {
                encrypted = val.parse().map_err(|e| {
                    Error::InvalidFormat(format!(
                        "invalid encrypted field '{}': {}", val, e
                    ))
                })?;
            }
            "KeyCaseSensitive" => {
                key_case_sensitive = val.eq_ignore_ascii_case("yes");
            }
            "UUID" => uuid = Some(val.into_bytes()),
            _ => {}
        }
    }

    Ok(MdictHeader {
        version, encoding, format, title, description,
        encrypted, key_case_sensitive, keyword_sect_start, uuid,
    })
}

pub(crate) fn decode_utf16le(data: &[u8]) -> crate::Result<String> {
    if data.len() % 2 != 0 {
        return Err(Error::InvalidFormat("odd byte count for UTF-16LE".into()));
    }
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&u16s)
        .map_err(|e| Error::InvalidFormat(format!("invalid UTF-16LE: {}", e)))
}

// parse_xml_attrs — unchanged (returns Vec, no Result)
```

---

## 12. `src/mdict/keys.rs`

Replace `use anyhow::anyhow` with `use crate::error::Error`. Map error variants:

```rust
use crate::error::Error;
use crate::mdict::header::MdictHeader;
use crate::mdict::decompress;

pub fn parse_keywords(
    data: &[u8],
    header: &MdictHeader,
    global_key: Option<&[u8; 16]>,
) -> crate::Result<(Vec<String>, Vec<u64>, usize)> {
    let mut pos = header.keyword_sect_start;

    if header.encrypted & 1 != 0 {
        return Err(Error::Unsupported(
            "encrypted keyword header not yet supported".into(),
        ));
    }

    if pos + 44 > data.len() {
        return Err(Error::InvalidFormat(
            "keyword section header truncated".into(),
        ));
    }

    // ... same field reads ...

    let computed = adler2::adler32_slice(&data[header_start..header_start + 40]);
    if computed != stored_checksum {
        return Err(Error::InvalidFormat(format!(
            "keyword section header checksum mismatch: stored={:#010x} computed={:#010x}",
            stored_checksum, computed
        )));
    }

    // ... key index decrypt block ...
    // All anyhow!(...) become Error::InvalidFormat(...)
    // e.g.:
    if block.len() < 8 {
        return Err(Error::InvalidFormat(
            "keyword index block too small to decrypt".into(),
        ));
    }

    // ... rest of function unchanged structurally ...
}

fn parse_key_index(
    data: &[u8],
    num_blocks: usize,
    header: &MdictHeader,
) -> crate::Result<Vec<(u64, u64)>> {
    // All anyhow!(...) → Error::InvalidFormat(format!(...))
    // Otherwise unchanged
}

fn parse_key_block(
    data: &[u8],
    header: &MdictHeader,
    keywords: &mut Vec<String>,
    record_offsets: &mut Vec<u64>,
) -> crate::Result<()> {
    let mut pos = 0;
    let nw = super::encoding::null_width(&header.encoding);

    while pos < data.len() {
        // Change: break → error
        if pos + 8 > data.len() {
            return Err(Error::InvalidFormat(
                "key block truncated: not enough data for record offset".into(),
            ));
        }

        // ... rest unchanged ...
    }

    Ok(())
}
```

---

## 13. `src/mdict/records.rs`

```rust
use crate::error::Error;
use crate::mdict::header::MdictHeader;

pub fn parse_record_index(
    data: &[u8],
    start: usize,
    _header: &MdictHeader,
) -> crate::Result<(Vec<(u64, u64, u64)>, u64)> {
    let mut pos = start;

    if pos + 32 > data.len() {
        return Err(Error::InvalidFormat(
            "record section header truncated".into(),
        ));
    }

    // ... field reads unchanged ...

    for _ in 0..num_blocks {
        if pos + 16 > data.len() {
            return Err(Error::InvalidFormat("record index truncated".into()));
        }
        // ... unchanged ...
    }

    Ok((blocks, record_blocks_start))
}
```

---

## 14. `src/mdict/file.rs`

Change return types, replace `.ok()?` with `?`, recover from mutex poison:

```rust
use std::collections::HashMap;
use std::fs::File;
use std::sync::{Arc, Mutex};

use memmap2::Mmap;

use super::{header, keys, records, keygen, decompress};
use super::header::MdictHeader;

// ... MdictFile struct unchanged ...

impl MdictFile {
    pub fn open(path: &std::path::Path, case_sensitive_override: Option<bool>) -> crate::Result<Self> {
        // body unchanged — io::Error and sub-parser errors all convert
    }

    fn get_block(&self, block_idx: usize) -> crate::Result<Arc<Vec<u8>>> {
        let mut cache = self.block_cache.lock().unwrap_or_else(|e| e.into_inner());
        // rest unchanged
    }

    pub fn lookup_raw(&self, key: &str) -> crate::Result<Option<Vec<u8>>> {
        let &i = match self.keyword_map.get(key) {
            Some(i) => i,
            None => return Ok(None),
        };
        let offset = self.record_offsets[i];
        let record_end = if i + 1 < self.record_offsets.len() {
            self.record_offsets[i + 1]
        } else {
            *self.decompressed_offsets.last().unwrap()
        };
        let record_len = (record_end - offset) as usize;

        let block_idx = self.decompressed_offsets
            .partition_point(|&off| off <= offset)
            .saturating_sub(1);

        let block_start = self.decompressed_offsets[block_idx];
        let block_end = self.decompressed_offsets[block_idx + 1];

        // Fast path
        if offset + record_len as u64 <= block_end {
            let block = self.get_block(block_idx)?;     // was .ok()?
            let local_start = (offset - block_start) as usize;
            let local_end = local_start + record_len;
            let mut result = block[local_start..local_end].to_vec();
            if result.last() == Some(&0) { result.pop(); }
            return Ok(Some(result));                      // was Some(result)
        }

        // Slow path
        let mut result = Vec::with_capacity(record_len);
        let mut bi = block_idx;
        while result.len() < record_len && bi < self.record_blocks.len() {
            let bs = self.decompressed_offsets[bi];
            let be = self.decompressed_offsets[bi + 1];
            let block = self.get_block(bi)?;              // was .ok()?
            let local_start = offset.saturating_sub(bs) as usize;
            let local_end = ((offset + record_len as u64) - bs).min(be - bs) as usize;
            result.extend_from_slice(&block[local_start..local_end]);
            bi += 1;
        }
        if result.last() == Some(&0) { result.pop(); }
        Ok(Some(result))                                  // was Some(result)
    }
}
```

---

## 15. `src/mdict/mod.rs`

Full replacement:

```rust
pub mod header;
pub mod keys;
pub mod records;
pub mod decompress;
pub mod ripemd128;
pub mod decrypt;
pub mod encoding;
pub mod keygen;
pub mod file;

use std::path;

use crate::error::Error;
use crate::types::{DictEntry, DictInfo};
use crate::Dictionary;

pub struct MdictDictionary {
    info_data: DictInfo,
    mdx: file::MdictFile,
    mdd: Vec<file::MdictFile>,
    format_type: char,
    case_sensitive: bool,
    encoding: String,
    sorted_keys: Vec<(String, usize)>,
}

impl MdictDictionary {
    pub fn open(dir: path::PathBuf) -> crate::Result<Self> {
        let mdx_path = find_mdx(&dir)?;
        let mdx = file::MdictFile::open(&mdx_path, None)?;

        let format_type = if mdx.header.format.eq_ignore_ascii_case("html") {
            'h'
        } else {
            'm'
        };
        let case_sensitive = mdx.header.key_case_sensitive;
        let encoding = mdx.header.encoding.clone();

        let info_data = DictInfo {
            name: mdx.header.title.clone(),
            author: String::new(),
            description: mdx.header.description.clone(),
            word_count: mdx.keywords.len() as u64,
        };

        let mut sorted_keys: Vec<(String, usize)> = mdx
            .keywords
            .iter()
            .enumerate()
            .map(|(i, k)| (k.to_lowercase(), i))
            .collect();
        sorted_keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let mdd = load_mdd_files(&mdx_path);

        Ok(MdictDictionary {
            info_data, mdx, mdd, format_type,
            case_sensitive, encoding, sorted_keys,
        })
    }

    pub fn lookup_resource(&self, path: &str) -> Option<Vec<u8>> {
        let normalised = path.replace('/', "\\");
        let lookup_key = normalised.to_lowercase();
        for mdd in &self.mdd {
            if let Ok(Some(data)) = mdd.lookup_raw(&lookup_key) {
                return Some(data);
            }
        }
        None
    }

    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        // unchanged
    }
}

impl Dictionary for MdictDictionary {
    fn lookup(&self, word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        let lookup_key = if self.case_sensitive {
            word.to_string()
        } else {
            word.to_lowercase()
        };
        let record_data = match self.mdx.lookup_raw(&lookup_key)? {
            Some(data) => data,
            None => return Ok(None),
        };
        let decoded = encoding::decode_str(&record_data, &self.encoding);
        Ok(Some(vec![DictEntry {
            type_id: self.format_type,
            data: decoded.into_bytes(),
        }]))
    }

    fn lookup_synonym(&self, _word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        Ok(None)
    }

    fn word_list(&self) -> Vec<String> {
        self.mdx.keywords.clone()
    }

    fn word_count(&self) -> usize {
        self.mdx.keywords.len()
    }

    fn info(&self) -> &DictInfo {
        &self.info_data
    }

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.search_prefix(prefix, limit)
    }
}

fn find_mdx(dir: &path::Path) -> crate::Result<path::PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().map_or(false, |e| e.eq_ignore_ascii_case("mdx")) {
            return Ok(path);
        }
    }
    Err(Error::InvalidFormat(format!(
        "no .mdx file found in {}", dir.display()
    )))
}

fn load_mdd_files(mdx_path: &std::path::Path) -> Vec<file::MdictFile> {
    // ... same body, except:
    // Err(e) => eprintln!("Warning: failed to load MDD {}: {}", path.display(), e),
    // becomes:
    // Err(e) => log::warn!("failed to load MDD {}: {}", path.display(), e),
}
```

---

## 16. `src/mdict/encoding.rs`

Add doc comment only — no functional changes:

```rust
/// Decodes bytes from the source encoding to a UTF-8 String.
/// Uses lossy conversion: invalid sequences become U+FFFD replacement
/// characters. This is intentional -- showing a definition with a few
/// bad characters is better than failing the entire lookup.
pub fn decode_str(bytes: &[u8], encoding_name: &str) -> String {
```

---

## 17. `tests/integration.rs`

Every `dict.lookup(...)` unwrap chain gains an extra `.unwrap()` for the `Result` layer. The `info` test switches from `dict.info()` (which returned `&Ifo`) to `dict.ifo()`:

```rust
use std::path::PathBuf;

use opendict::stardict::StarDictDictionary as Dictionary;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[test]
fn load_dictionary_from_fixtures() {
    let dict = Dictionary::open(fixtures_dir(), "testdict");
    assert!(dict.is_ok(), "Should load dictionary from fixture files");
}

#[test]
fn list_all_words_from_index() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let words = dict.word_list();
    assert_eq!(words, vec!["another", "foo", "lorem", "some word"]);
}

#[test]
fn lookup_foo_returns_bar() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let results = dict.lookup("foo").unwrap().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].type_id, 'm');
    assert_eq!(std::str::from_utf8(&results[0].data).unwrap(), "bar");
}

#[test]
fn lookup_another_returns_translat() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let results = dict.lookup("another").unwrap().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].type_id, 'm');
    assert_eq!(
        std::str::from_utf8(&results[0].data).unwrap(),
        "translat"
    );
}

#[test]
fn lookup_some_word_returns_a_translation() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let results = dict.lookup("some word").unwrap().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].type_id, 'm');
    assert_eq!(
        std::str::from_utf8(&results[0].data).unwrap(),
        "a translation"
    );
}

#[test]
fn lookup_nonexistent_returns_empty() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let result = dict.lookup("nonexistent").unwrap();
    assert!(result.is_none(), "Nonexistent word should return None");
}

#[test]
fn synonym_abc_resolves_to_some_word() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let results = dict.lookup_synonym("abc").unwrap().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].type_id, 'm');
    assert_eq!(
        std::str::from_utf8(&results[0].data).unwrap(),
        "a translation"
    );
}

#[test]
fn info_returns_correct_metadata() {
    let dict = Dictionary::open(fixtures_dir(), "testdict").unwrap();
    let ifo = dict.ifo();
    assert_eq!(ifo.name, "A foo-bar dictionary");
    assert_eq!(ifo.version, "3.0.0");
    assert_eq!(ifo.word_count, 4);
    assert_eq!(ifo.idx_file_size, 60);
    assert_eq!(ifo.same_type_sequence, "m");
}

#[test]
fn load_multitype_dictionary() {
    let dict = Dictionary::open(fixtures_dir(), "multitype");
    assert!(
        dict.is_ok(),
        "Should load multitype dictionary (no sametypesequence)"
    );
}

#[test]
fn multitype_lookup_hello() {
    let dict = Dictionary::open(fixtures_dir(), "multitype").unwrap();
    let results = dict.lookup("hello").unwrap().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].type_id, 'h');
    assert_eq!(results[0].data, b"<b>hello</b>");
    assert_eq!(results[1].type_id, 't');
    assert_eq!(results[1].data, b"helo");
}

#[test]
fn multitype_lookup_world() {
    let dict = Dictionary::open(fixtures_dir(), "multitype").unwrap();
    let results = dict.lookup("world").unwrap().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].type_id, 'm');
    assert_eq!(results[0].data, b"the world");
}
```

---

## 18. `tests/mdict_fixture.rs`

`lookup_raw` now returns `Result<Option<_>>`:

```rust
// Lookup tests — add .unwrap() for Result, keep .unwrap() for Option:
#[test]
fn lookup_foo() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    let data = mdx.lookup_raw("foo").unwrap().unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), "bar");
}

#[test]
fn lookup_hello() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    let data = mdx.lookup_raw("hello").unwrap().unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), "<b>hello</b> greeting");
}

#[test]
fn lookup_test() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    let data = mdx.lookup_raw("test").unwrap().unwrap();
    assert_eq!(String::from_utf8(data).unwrap(), "test data here");
}

#[test]
fn lookup_miss() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    assert!(mdx.lookup_raw("nonexistent").unwrap().is_none());
}

#[test]
fn lookup_case_insensitive() {
    let mdx = MdictFile::open(&fixture_path(), None).unwrap();
    assert!(mdx.lookup_raw("FOO").unwrap().is_none());
    assert!(mdx.lookup_raw("foo").unwrap().is_some());
}

// Full dictionary tests:
#[test]
fn full_dictionary_lookup() {
    // ...
    let result = dict.lookup("foo").unwrap().unwrap();
    // ...
}
```

---

## 19. `tests/real_dicts.rs`

Update `lookup` call sites for `Result<Option<_>>`:

```rust
// StarDict lookup tests:
let result = dict.lookup(&words[0]).unwrap();
assert!(
    result.is_some() && !result.as_ref().unwrap().is_empty(),
    // ...
);

// "is_none" checks:
if dict.lookup(word).unwrap().is_none() {
    failures += 1;
}

// MDict load test — error matching still works:
Err(e) => {
    let msg = e.to_string();
    if msg.contains("unsupported") || msg.contains("LZO") {
        // ...
    }
}

// MDict lookup tests — same pattern:
let result = dict.lookup(&words[0]).unwrap();
assert!(
    result.is_some() && !result.as_ref().unwrap().is_empty(),
    // ...
);

if dict.lookup(word).unwrap().is_none() {
    failures += 1;
}
```

Helper functions:
```rust
fn mdict_lookup(subdir: &str, word: &str) -> Option<String> {
    let dir = mdict_dir().join(subdir);
    let dict = opendict::open(dir).ok()?;
    let entries = dict.lookup(word).ok()??;   // Result then Option
    // ...
}

fn stardict_lookup(subdir: &str, word: &str) -> Option<String> {
    let dir = stardict_dir().join(subdir);
    let dict = opendict::open(dir).ok()?;
    let entries = dict.lookup(word).ok()??;   // Result then Option
    // ...
}
```

---

## 20. `examples/lookup.rs`

```rust
fn main() {
    // ... args parsing unchanged ...

    let dict = opendict::open(&args[1]).unwrap_or_else(|e| {
        eprintln!("Failed to open dictionary: {e}");
        process::exit(1);
    });

    let info = dict.info();
    println!("{} ({} words)\n", info.name, dict.word_count());

    match dict.lookup(&args[2]) {
        Ok(Some(entries)) => {
            for e in &entries {
                let text = String::from_utf8_lossy(&e.data);
                let preview: String = text.chars().take(200).collect();
                let ellipsis = if text.chars().count() > 200 { "..." } else { "" };
                println!("  [{}] {}{}", e.type_id, preview.trim(), ellipsis);
            }
        }
        Ok(None) => println!("  (not found)"),
        Err(e) => eprintln!("  Error: {e}"),
    }
}
```

---

## 21. `examples/show_dict.rs`

```rust
fn main() {
    // ... unchanged until lookup ...

    for i in indices {
        let word = &words[i];
        print!("  [{i}] {word:?}");
        match dict.lookup(word) {
            Ok(Some(entries)) => {
                for e in &entries {
                    let text = String::from_utf8_lossy(&e.data);
                    let preview: String = text.chars().take(120).collect();
                    let ellipsis = if text.chars().count() > 120 { "..." } else { "" };
                    print!("  [{}] {}{}", e.type_id, preview.trim(), ellipsis);
                }
                println!();
            }
            Ok(None) => println!("  NOT FOUND"),
            Err(e) => println!("  ERROR: {e}"),
        }
    }
}
```

---

## 22. `benches/dictionary.rs`

No changes needed. `black_box(dict.lookup(w))` accepts any `T`, and the benchmark doesn't destructure the return value. Compiles as-is with the new `Result<Option<_>>` return type.

---

## 23. `mobile/src/lib.rs`

Three breaking changes: `lookup`/`lookup_synonym` return `Result<Option<_>>` now, and `info()` returns `&DictInfo` instead of `DictInfo`.

Add a `LookupError` variant to `DictError`. Update `lookup`/`lookup_synonym` to propagate the `Result`. Update `info()` to clone from the reference.

```rust
use std::path::PathBuf;
use std::sync::Arc;

uniffi::setup_scaffolding!();

#[derive(uniffi::Record)]
pub struct DictEntry {
    pub entry_type: String,
    pub data: String,
}

#[derive(uniffi::Record)]
pub struct DictInfo {
    pub name: String,
    pub author: String,
    pub description: String,
    pub word_count: u64,
}

#[derive(uniffi::Enum)]
pub enum DictKind {
    StarDict,
    MDict,
}

#[derive(Debug, uniffi::Error)]
pub enum DictError {
    LoadError { message: String },
    LookupError { message: String },
}

impl std::fmt::Display for DictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DictError::LoadError { message } => write!(f, "{}", message),
            DictError::LookupError { message } => write!(f, "{}", message),
        }
    }
}

#[derive(uniffi::Object)]
pub struct Dictionary {
    inner: Box<dyn opendict::Dictionary + Send + Sync>,
}

#[uniffi::export]
impl Dictionary {
    #[uniffi::constructor]
    fn new(dir: String, kind: DictKind) -> Result<Arc<Self>, DictError> {
        let k = match kind {
            DictKind::StarDict => opendict::DictKind::StarDict,
            DictKind::MDict => opendict::DictKind::MDict,
        };
        let inner = opendict::open(PathBuf::from(&dir), k)
            .map_err(|e| DictError::LoadError { message: e.to_string() })?;
        Ok(Arc::new(Dictionary { inner }))
    }

    fn lookup(&self, word: String) -> Result<Option<Vec<DictEntry>>, DictError> {
        let result = self.inner.lookup(&word)
            .map_err(|e| DictError::LookupError { message: e.to_string() })?;
        Ok(result.map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        }))
    }

    fn lookup_synonym(&self, word: String) -> Result<Option<Vec<DictEntry>>, DictError> {
        let result = self.inner.lookup_synonym(&word)
            .map_err(|e| DictError::LookupError { message: e.to_string() })?;
        Ok(result.map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        }))
    }

    fn word_list(&self) -> Vec<String> {
        self.inner.word_list()
    }

    fn word_count(&self) -> u32 {
        self.inner.word_count() as u32
    }

    fn info(&self) -> DictInfo {
        let i = self.inner.info();
        DictInfo {
            name: i.name.clone(),
            author: i.author.clone(),
            description: i.description.clone(),
            word_count: i.word_count,
        }
    }
}
```

Changes vs current:
- `DictError` gains `LookupError { message: String }`
- `lookup` returns `Result<Option<Vec<DictEntry>>, DictError>` instead of `Option<Vec<DictEntry>>`
- `lookup_synonym` same change
- `info()` clones fields from `&DictInfo` reference instead of moving

---

## 24. `expo/ios/OpenDictModule.swift`

`lookup` and `lookupSynonym` now throw on the uniffi side (because they return `Result`). The Swift module needs `try` to handle errors, converting them to `nil` for JS:

```swift
import ExpoModulesCore

public class OpenDictModule: Module {
    private var dictionaries: [Int: Dictionary] = [:]
    private var nextHandle = 1

    public func definition() -> ModuleDefinition {
        Name("OpenDict")

        Function("open") { (dir: String, name: String) -> Int in
            let dict = try Dictionary(dir: dir, name: name)
            let handle = self.nextHandle
            self.nextHandle += 1
            self.dictionaries[handle] = dict
            return handle
        }

        Function("close") { (handle: Int) in
            self.dictionaries.removeValue(forKey: handle)
        }

        Function("lookup") { (handle: Int, word: String) -> [[String: String]]? in
            guard let dict = self.dictionaries[handle] else { return nil }
            guard let entries = try? dict.lookup(word: word) else { return nil }
            return entries.map { ["entryType": $0.entryType, "data": $0.data] }
        }

        Function("lookupSynonym") { (handle: Int, word: String) -> [[String: String]]? in
            guard let dict = self.dictionaries[handle] else { return nil }
            guard let entries = try? dict.lookupSynonym(word: word) else { return nil }
            return entries.map { ["entryType": $0.entryType, "data": $0.data] }
        }

        Function("wordList") { (handle: Int) -> [String] in
            guard let dict = self.dictionaries[handle] else { return [] }
            return dict.wordList()
        }

        Function("wordCount") { (handle: Int) -> Int in
            guard let dict = self.dictionaries[handle] else { return 0 }
            return Int(dict.wordCount())
        }

        Function("getName") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.name()
        }

        Function("getAuthor") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.author()
        }

        Function("getDescription") { (handle: Int) -> String in
            guard let dict = self.dictionaries[handle] else { return "" }
            return dict.description()
        }
    }
}
```

Changes: `lookup` and `lookupSynonym` use `try?` — errors become `nil` to JS (same as "not found"). This matches the current JS interface (`DictEntry[] | null`) so no TS changes needed.

---

## 25. `expo/android/src/main/java/expo/modules/opendict/OpenDictModule.kt`

Same issue — `lookup`/`lookupSynonym` now throw. Wrap in try-catch, returning `null` on error:

```kotlin
package expo.modules.opendict

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import uniffi.opendict_mobile.Dictionary
import uniffi.opendict_mobile.DictEntry

class OpenDictModule : Module() {
    private val dictionaries = mutableMapOf<Int, Dictionary>()
    private var nextHandle = 1

    override fun definition() = ModuleDefinition {
        Name("OpenDict")

        Function("open") { dir: String, name: String ->
            val dict = Dictionary(dir, name)
            val handle = nextHandle++
            dictionaries[handle] = dict
            handle
        }

        Function("close") { handle: Int ->
            dictionaries.remove(handle)
        }

        Function("lookup") { handle: Int, word: String ->
            try {
                dictionaries[handle]?.lookup(word)?.map {
                    mapOf("entryType" to it.entryType, "data" to it.data)
                }
            } catch (e: Exception) {
                null
            }
        }

        Function("lookupSynonym") { handle: Int, word: String ->
            try {
                dictionaries[handle]?.lookupSynonym(word)?.map {
                    mapOf("entryType" to it.entryType, "data" to it.data)
                }
            } catch (e: Exception) {
                null
            }
        }

        Function("wordList") { handle: Int ->
            dictionaries[handle]?.wordList() ?: emptyList()
        }

        Function("wordCount") { handle: Int ->
            dictionaries[handle]?.wordCount()?.toInt() ?: 0
        }

        Function("getName") { handle: Int ->
            dictionaries[handle]?.name() ?: ""
        }

        Function("getAuthor") { handle: Int ->
            dictionaries[handle]?.author() ?: ""
        }

        Function("getDescription") { handle: Int ->
            dictionaries[handle]?.description() ?: ""
        }
    }
}
```

Changes: `lookup` and `lookupSynonym` wrapped in try-catch, returning `null` on exception.

---

## 26. Unhappy path tests

New tests validating correct error variants. Uses `matches!` to check the variant, not just `is_err()`.

### `src/stardict/ifo.rs` — add to existing `#[cfg(test)]` module

```rust
#[test]
fn nonexistent_file_is_io_error() {
    let result = Ifo::open(PathBuf::from("/nonexistent/path.ifo"));
    assert!(matches!(result, Err(crate::error::Error::Io(_))));
}

#[test]
fn wrong_magic_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.ifo");
    std::fs::write(&path, "Wrong magic\nversion=3.0.0\nbookname=X\nwordcount=1\nidxfilesize=10\n").unwrap();
    let result = Ifo::open(path);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn unsupported_version_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad_ver.ifo");
    std::fs::write(&path, "StarDict's dict ifo file\nversion=1.0.0\nbookname=X\nwordcount=1\nidxfilesize=10\n").unwrap();
    let result = Ifo::open(path);
    assert!(matches!(result, Err(crate::error::Error::Unsupported(_))));
}

#[test]
fn missing_bookname_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no_name.ifo");
    std::fs::write(&path, "StarDict's dict ifo file\nversion=3.0.0\nwordcount=1\nidxfilesize=10\n").unwrap();
    let result = Ifo::open(path);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn invalid_wordcount_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad_wc.ifo");
    std::fs::write(&path, "StarDict's dict ifo file\nversion=3.0.0\nbookname=X\nwordcount=abc\nidxfilesize=10\n").unwrap();
    let result = Ifo::open(path);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}
```

### `src/stardict/idx.rs` — add to existing `#[cfg(test)]` module

```rust
#[test]
fn truncated_idx_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trunc.idx");
    // Word "hi" + null + only 4 bytes (needs 8 for 32-bit offset+size)
    std::fs::write(&path, b"hi\x00\x00\x00\x00\x00").unwrap();
    let result = Idx::open(path, 32);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn invalid_utf8_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("badutf8.idx");
    // Invalid UTF-8 byte 0xFF, then null, then 8 bytes of offset+size
    let mut data = vec![0xFF, 0x00];
    data.extend_from_slice(&[0u8; 8]);
    std::fs::write(&path, &data).unwrap();
    let result = Idx::open(path, 32);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn missing_null_terminator_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonull.idx");
    // "hello" with no null terminator — parser will hit EOF
    std::fs::write(&path, b"hello").unwrap();
    let result = Idx::open(path, 32);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn nonexistent_idx_is_io_error() {
    let result = Idx::open(PathBuf::from("/nonexistent/test.idx"), 32);
    assert!(matches!(result, Err(crate::error::Error::Io(_))));
}
```

### `src/stardict/dict.rs` — add to existing `#[cfg(test)]` module

```rust
#[test]
fn read_entry_past_eof_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.dict");
    std::fs::write(&path, b"hello").unwrap();
    let dict = Dict::open(path, false).unwrap();
    let result = dict.read_entry(0, 100, Some("m"));
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn parse_entries_missing_null_is_invalid_format() {
    // sametypesequence "tm": first field 't' needs null terminator
    // but data has no null byte
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonull.dict");
    std::fs::write(&path, b"no null here").unwrap();
    let dict = Dict::open(path, false).unwrap();
    let result = dict.read_entry(0, 12, Some("tm"));
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}
```

### `src/mdict/decompress.rs` — add `#[cfg(test)]` module

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_too_small_is_invalid_format() {
        let result = decompress_block(&[0; 4], 2.0, None);
        assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
    }

    #[test]
    fn lzo_compression_is_unsupported() {
        // info field: compression=1 (LZO), encryption=0, size=0
        let mut block = vec![0x01, 0x00, 0x00, 0x00]; // info LE
        block.extend_from_slice(&[0x00; 4]); // checksum
        block.push(0x00); // minimal data
        let result = decompress_block(&block, 2.0, None);
        assert!(matches!(result, Err(crate::error::Error::Unsupported(_))));
    }

    #[test]
    fn unknown_encryption_is_unsupported() {
        // info field: compression=0, encryption=2 (salsa20)
        let mut block = vec![0x20, 0x00, 0x00, 0x00]; // info LE: enc=2
        block.extend_from_slice(&[0x00; 4]); // checksum
        let result = decompress_block(&block, 2.0, None);
        assert!(matches!(result, Err(crate::error::Error::Unsupported(_))));
    }

    #[test]
    fn bad_checksum_is_invalid_format() {
        // Uncompressed block (compression=0, encryption=0) with wrong checksum
        let mut block = vec![0x00, 0x00, 0x00, 0x00]; // info: no compression
        block.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // bad checksum
        block.extend_from_slice(b"hello"); // data
        let result = decompress_block(&block, 2.0, None);
        assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
    }
}
```

### `src/mdict/header.rs` — add to existing `#[cfg(test)]` module

```rust
#[test]
fn too_small_is_invalid_format() {
    let result = parse_header(&[0; 4]);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}

#[test]
fn truncated_header_is_invalid_format() {
    // Claim header is 1000 bytes but only provide 20
    let mut data = vec![0; 20];
    data[3] = 200; // header_len = 200, but data is only 20 bytes
    let result = parse_header(&data);
    assert!(matches!(result, Err(crate::error::Error::InvalidFormat(_))));
}
```

### `tests/integration.rs` — add at the end

```rust
// ── Error variant tests ─────────────────────────────────────────────

#[test]
fn open_nonexistent_dir_is_io_error() {
    let result = opendict::open("/nonexistent/path/to/dict");
    assert!(matches!(result, Err(opendict::Error::Io(_))));
}

#[test]
fn open_empty_dir_is_invalid_format() {
    let dir = tempfile::tempdir().unwrap();
    let result = opendict::open(dir.path());
    assert!(matches!(result, Err(opendict::Error::InvalidFormat(_))));
}
```

---

## Verification

1. `cargo check` — all modules compile
2. `cargo test` — unit tests and integration tests pass (including new unhappy-path tests)
3. `cargo test -- --ignored` — real dictionary tests pass (if dicts present)
4. `cargo build --examples` — examples compile
5. `cargo bench --no-run` — benchmarks compile
6. `cargo tree -i anyhow` — not present at all

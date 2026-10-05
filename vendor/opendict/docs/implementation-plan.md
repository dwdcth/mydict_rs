# StarDict Implementation Plan

## Current State

23 tests pass (IFO basic parsing, fixture byte checks), **64 tests fail** on `todo!()` stubs.
6 IFO tests fail due to missing validation (magic line, required fields, whitespace trimming).

## Implementation Order

Bottom-up — each module only depends on the ones above it:

1. **strcmp.rs** — no dependencies, needed by idx search
2. **ifo.rs** — fix 6 failing validation tests
3. **idx.rs** — depends on strcmp
4. **dict.rs** — no dependencies
5. **syn.rs** — no dependencies
6. **dictionary.rs** — already wired up, just needs the above to work

---

## 1. `src/strcmp.rs` — stardict_strcmp

Replace the `todo!()` with the spec algorithm, ported from JS `common.js:25-66`.

```rust
use std::cmp::Ordering;

pub fn stardict_strcmp(s1: &str, s2: &str) -> Ordering {
    let b1 = s1.as_bytes();
    let b2 = s2.as_bytes();

    // Phase 1: g_ascii_strcasecmp — case-insensitive for ASCII A-Z only
    let common_len = b1.len().min(b2.len());
    for i in 0..common_len {
        let c1 = ascii_lower(b1[i]);
        let c2 = ascii_lower(b2[i]);
        if c1 != c2 {
            return c1.cmp(&c2);
        }
    }
    let case_cmp = b1.len().cmp(&b2.len());
    if case_cmp != Ordering::Equal {
        return case_cmp;
    }

    // Phase 2: raw byte strcmp tiebreaker
    for i in 0..common_len {
        if b1[i] != b2[i] {
            return b1[i].cmp(&b2[i]);
        }
    }
    b1.len().cmp(&b2.len())
}

fn ascii_lower(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' {
        b + (b'a' - b'A')
    } else {
        b
    }
}
```

**Key insight**: operates on raw UTF-8 bytes, not characters. Non-ASCII bytes (>127) pass through untouched. This matches the JS version which uses `TextEncoder` to get UTF-8 bytes, then compares byte values.

**Tests**: 18 tests in `tests/strcmp.rs`

---

## 2. `src/ifo.rs` — Fix 6 failing tests

The struct and field parsing already work. Add three things:

### a) Magic line validation

First line must be exactly `"StarDict's dict ifo file"`. Read lines, check first non-empty line.

### b) Required field validation

After parsing, check `has_name && has_word_count && has_idx_file_size`. Return error if any missing.

### c) Whitespace trimming

Trim key and value around `=`: `let key = line[..id].trim()` and `let val = line[id+1..].trim()`.

```rust
use std::io::BufRead;
use std::{fs, io, path};

use super::result::Result;

#[derive(Debug, Clone)]
pub struct Ifo {
    pub author: String,
    pub version: String,
    pub name: String,
    pub date: String,
    pub description: String,
    pub email: String,
    pub web_site: String,
    pub same_type_sequence: String,
    pub idx_file_size: u64,
    pub word_count: u64,
    pub syn_word_count: u64,
    pub idx_offset_bits: u32,
}

impl Ifo {
    pub fn open(file: path::PathBuf) -> Result<Ifo> {
        let mut it = Ifo {
            author: String::new(),
            version: String::new(),
            name: String::new(),
            date: String::new(),
            description: String::new(),
            email: String::new(),
            web_site: String::new(),
            same_type_sequence: String::new(),
            idx_file_size: 0,
            word_count: 0,
            syn_word_count: 0,
            idx_offset_bits: 32,
        };

        let mut has_name = false;
        let mut has_word_count = false;
        let mut has_idx_file_size = false;
        let mut magic_checked = false;

        for line in io::BufReader::new(fs::File::open(file)?).lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if !magic_checked {
                if line != "StarDict's dict ifo file" {
                    return Err(format_err!("invalid magic line: {}", line));
                }
                magic_checked = true;
                continue;
            }

            if let Some(id) = line.find('=') {
                let key = line[..id].trim();
                let val = line[id + 1..].trim().to_string();
                match key {
                    "author" => it.author = val,
                    "bookname" => {
                        it.name = val;
                        has_name = true;
                    }
                    "version" => {
                        match val.as_str() {
                            "2.4.2" | "3.0.0" => it.version = val,
                            v => return Err(format_err!("unsupported version: {}", v)),
                        }
                    }
                    "description" => it.description = val,
                    "date" => it.date = val,
                    "idxfilesize" => {
                        it.idx_file_size = val.parse()?;
                        has_idx_file_size = true;
                    }
                    "wordcount" => {
                        it.word_count = val.parse()?;
                        has_word_count = true;
                    }
                    "website" => it.web_site = val,
                    "email" => it.email = val,
                    "sametypesequence" => it.same_type_sequence = val,
                    "synwordcount" => it.syn_word_count = val.parse()?,
                    "idxoffsetbits" => it.idx_offset_bits = val.parse()?,
                    _ => warn!("Ignore line: {}", line),
                };
            }
        }

        if !has_name {
            return Err(format_err!("missing required field: bookname"));
        }
        if !has_word_count {
            return Err(format_err!("missing required field: wordcount"));
        }
        if !has_idx_file_size {
            return Err(format_err!("missing required field: idxfilesize"));
        }

        Ok(it)
    }
}
```

**Tests**: 26 tests in `tests/ifo.rs` (20 already pass, 6 will be fixed)

---

## 3. `src/idx.rs` — IDX binary parsing

Ported from JS `IdxIter` class in `common.js:85-140`.

Binary format per entry:
- `word_str` — null-terminated UTF-8 string
- `offset` — u32 (4 bytes) or u64 (8 bytes) big-endian, per `idxoffsetbits`
- `size` — u32 (4 bytes) big-endian

```rust
use std::{fs, path, str};

use super::result::Result;
use super::strcmp::stardict_strcmp;

#[derive(Debug, Clone)]
pub struct IdxEntry {
    pub word: String,
    pub offset: u64,
    pub size: u32,
}

pub struct Idx {
    entries: Vec<IdxEntry>,
}

impl Idx {
    pub fn open(file: path::PathBuf, _word_count: u32, offset_bits: u32) -> Result<Idx> {
        let data = fs::read(&file)?;
        let mut entries = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            // Find null terminator for word
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| format_err!("idx: missing null terminator at offset {}", pos))?;
            let null_pos = pos + null_pos;

            let word = str::from_utf8(&data[pos..null_pos])?.to_string();
            let ref_start = null_pos + 1;

            let (offset, size) = if offset_bits == 64 {
                // 8 bytes offset + 4 bytes size
                if ref_start + 12 > data.len() {
                    return Err(format_err!("idx: unexpected EOF reading 64-bit entry"));
                }
                let offset = u64::from_be_bytes([
                    data[ref_start],     data[ref_start + 1],
                    data[ref_start + 2], data[ref_start + 3],
                    data[ref_start + 4], data[ref_start + 5],
                    data[ref_start + 6], data[ref_start + 7],
                ]);
                let size = u32::from_be_bytes([
                    data[ref_start + 8],  data[ref_start + 9],
                    data[ref_start + 10], data[ref_start + 11],
                ]);
                pos = ref_start + 12;
                (offset, size)
            } else {
                // 4 bytes offset + 4 bytes size
                if ref_start + 8 > data.len() {
                    return Err(format_err!("idx: unexpected EOF reading 32-bit entry"));
                }
                let offset = u32::from_be_bytes([
                    data[ref_start],     data[ref_start + 1],
                    data[ref_start + 2], data[ref_start + 3],
                ]) as u64;
                let size = u32::from_be_bytes([
                    data[ref_start + 4], data[ref_start + 5],
                    data[ref_start + 6], data[ref_start + 7],
                ]);
                pos = ref_start + 8;
                (offset, size)
            };

            entries.push(IdxEntry { word, offset, size });
        }

        Ok(Idx { entries })
    }

    pub fn entries(&self) -> &[IdxEntry] {
        &self.entries
    }

    pub fn search(&self, word: &str) -> Option<&IdxEntry> {
        self.entries
            .binary_search_by(|entry| stardict_strcmp(&entry.word, word))
            .ok()
            .map(|i| &self.entries[i])
    }
}
```

**Tests**: 12 tests in `tests/idx.rs`

---

## 4. `src/dict.rs` — DICT data reading

Ported from JS `_processEntryData` in `stardict_sync.js`.

Three modes:
1. **sametypesequence** (e.g. `"m"`): type sequence is predefined, data is just values. Last field consumes rest of data (no null terminator).
2. **sametypesequence multi** (e.g. `"tm"`): first field null-terminated, last field consumes rest.
3. **No sametypesequence**: each field prefixed with type byte. Lowercase = null-terminated, uppercase = u32 length prefix.

```rust
use std::{fs, path};

use super::result::Result;

#[derive(Debug, Clone)]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

pub struct Dict {
    data: Vec<u8>,
}

impl Dict {
    pub fn open(file: path::PathBuf) -> Result<Dict> {
        let data = fs::read(&file)?;
        Ok(Dict { data })
    }

    pub fn read_entry(&self, offset: u64, size: u32, sametypesequence: Option<&str>) -> Vec<DictEntry> {
        let start = offset as usize;
        let end = start + size as usize;
        let mut raw = &self.data[start..end];
        let mut entries = Vec::new();

        match sametypesequence {
            Some(sts) => {
                let types: Vec<char> = sts.chars().collect();
                for (i, &type_id) in types.iter().enumerate() {
                    let is_last = i == types.len() - 1;
                    if is_last {
                        // Last type: consumes rest of data, no terminator
                        entries.push(DictEntry {
                            type_id,
                            data: raw.to_vec(),
                        });
                        break;
                    }
                    // Non-last types: null-terminated (lowercase) or u32 length (uppercase)
                    if type_id.is_ascii_uppercase() {
                        let len = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
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
                        let len = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
                        entries.push(DictEntry {
                            type_id,
                            data: raw[4..4 + len].to_vec(),
                        });
                        raw = &raw[4 + len..];
                    } else {
                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
                        entries.push(DictEntry {
                            type_id,
                            data: raw[..null_pos].to_vec(),
                        });
                        raw = &raw[null_pos + 1..];
                    }
                }
            }
        }

        entries
    }
}
```

**Tests**: 11 tests in `tests/dict.rs`

---

## 5. `src/syn.rs` — SYN file parsing

Same binary format as IDX but simpler: `word\0` + `u32 index` (always 4 bytes, no offset_bits).

```rust
use std::{fs, path, str};

use super::result::Result;
use super::strcmp::stardict_strcmp;

#[derive(Debug, Clone)]
pub struct SynEntry {
    pub word: String,
    pub original_word_index: u32,
}

pub struct Syn {
    entries: Vec<SynEntry>,
}

impl Syn {
    pub fn open(file: path::PathBuf, _syn_word_count: u32) -> Result<Syn> {
        let data = fs::read(&file)?;
        let mut entries = Vec::new();
        let mut pos = 0;

        while pos < data.len() {
            let null_pos = data[pos..]
                .iter()
                .position(|&b| b == 0)
                .ok_or_else(|| format_err!("syn: missing null terminator at offset {}", pos))?;
            let null_pos = pos + null_pos;

            let word = str::from_utf8(&data[pos..null_pos])?.to_string();
            let idx_start = null_pos + 1;

            if idx_start + 4 > data.len() {
                return Err(format_err!("syn: unexpected EOF reading word index"));
            }
            let original_word_index = u32::from_be_bytes([
                data[idx_start],     data[idx_start + 1],
                data[idx_start + 2], data[idx_start + 3],
            ]);

            entries.push(SynEntry { word, original_word_index });
            pos = idx_start + 4;
        }

        Ok(Syn { entries })
    }

    pub fn entries(&self) -> &[SynEntry] {
        &self.entries
    }

    pub fn lookup(&self, word: &str) -> Option<&SynEntry> {
        self.entries.iter().find(|e| e.word == word)
    }
}
```

**Tests**: 9 tests in `tests/syn.rs`

---

## 6. `src/dictionary.rs` — No changes needed

Already wired up correctly. Once the above modules work, `Dictionary::open()`, `lookup()`, `word_list()`, `lookup_synonym()`, and `info()` will work because they delegate to `Idx`, `Dict`, `Syn`, and `Ifo`.

---

## Verification

```sh
cargo test                    # All 87 non-ignored tests should pass
cargo test -- --ignored       # Real dict tests (when dicts present in tests/dicts/)
```

## Files Changed

| File | Change |
|---|---|
| `src/strcmp.rs` | Full implementation (replace `todo!()`) |
| `src/ifo.rs` | Add magic line check, version validation, required field check, whitespace trim |
| `src/idx.rs` | Full implementation (replace `todo!()`) |
| `src/dict.rs` | Full implementation (replace `todo!()`) |
| `src/syn.rs` | Full implementation (replace `todo!()`) |
| `src/dictionary.rs` | No changes |
| `src/lib.rs` | No changes |

# Auto-Decompress .dict.dz Plan

## Problem

Dictzip random access decompresses a ~58KB chunk per lookup, costing ~60-80μs
each time. For interactive use this is fine, but it's unnecessary overhead when
we can just decompress once to disk and mmap the result forever after.

## What changes

Only `src/dict.rs` changes. The DictZip codepath is removed entirely. Instead,
when we encounter a `.dict.dz` file:

1. Check if a `.dict` file already exists alongside it — if so, mmap that
2. Otherwise, decompress the `.dz` to a `.dict` file next to it, then mmap it
3. If the write fails (read-only filesystem), fall back to keeping it in memory

This means first load pays the decompression cost once, and every subsequent
load is a fast mmap.

No other files change. The `storage_label()` method stays but loses the
"dictzip" variant — everything is either "mmapped" or "decompressed".

## File Changes

### 1. `src/dict.rs` — full replacement

```rust
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path;

use flate2::read::GzDecoder;
use memmap2::Mmap;

#[derive(Debug, Clone)]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

enum DictData {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

pub struct Dict {
    data: DictData,
}

impl Dict {
    pub fn open(file: path::PathBuf) -> anyhow::Result<Dict> {
        let is_compressed = file.extension()
            .map_or(false, |ext| ext == "gz" || ext == "dz");
        if is_compressed {
            // Check if a decompressed .dict already exists alongside
            let decompressed_path = file.with_extension("").with_extension("dict");
            if decompressed_path.exists() {
                return Self::open_mmap(&decompressed_path);
            }

            // Decompress and write to .dict, then mmap it
            let raw = fs::read(&file)?;
            let mut decoder = GzDecoder::new(&raw[..]);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;

            if let Ok(mut f) = File::create(&decompressed_path) {
                let _ = f.write_all(&decompressed);
                drop(f);
                return Self::open_mmap(&decompressed_path);
            }

            // If we can't write the file (read-only fs, etc.), keep in memory
            Ok(Dict { data: DictData::Owned(decompressed) })
        } else {
            Self::open_mmap(&file)
        }
    }

    fn open_mmap(file: &path::Path) -> anyhow::Result<Dict> {
        let f = File::open(file)?;
        // SAFETY: dictionary files are read-only; we do not modify them.
        let mmap = unsafe { Mmap::map(&f)? };
        Ok(Dict { data: DictData::Mapped(mmap) })
    }

    fn bytes(&self) -> &[u8] {
        match &self.data {
            DictData::Mapped(m) => m,
            DictData::Owned(v) => v,
        }
    }

    pub fn data_len(&self) -> usize {
        match &self.data {
            DictData::Mapped(_) => 0,
            DictData::Owned(v) => v.len(),
        }
    }

    pub fn storage_label(&self) -> &'static str {
        match &self.data {
            DictData::Mapped(_) => "mmapped",
            DictData::Owned(_) => "decompressed",
        }
    }

    pub fn read_entry(
        &self,
        offset: u64,
        size: u32,
        sametypesequence: Option<&str>,
    ) -> Vec<DictEntry> {
        let data = self.bytes();
        let start = offset as usize;
        let end = start + size as usize;
        let raw = &data[start..end];
        Self::parse_entries(raw, sametypesequence)
    }

    fn parse_entries(data: &[u8], sametypesequence: Option<&str>) -> Vec<DictEntry> {
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
                        let len =
                            u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
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

### 2. No other files change

- `examples/bench.rs` — already shows `storage_label()`, will now show "mmapped" for all dicts
- `src/dictionary.rs` — unchanged, calls `dict.read_entry()` as before
- All tests — unchanged
- `Cargo.toml` — unchanged, `flate2` still needed for the one-time decompression

### 3. Cleanup: remove unused dependency

The `flate2::Decompress` and `FlushDecompress` imports are no longer needed
(those were for chunk-level dictzip decompression). Only `flate2::read::GzDecoder`
remains for the one-time full decompression.

## Expected Results

All four dicts will be mmapped after first run. Second run onwards:

| Dictionary | Dict before | Dict after |
|---|---|---|
| 现代汉语词典 | 0 B (mmapped) | 0 B (mmapped) |
| 朗道汉英字典 | 3.8 MB (dictzip) | 0 B (mmapped) |
| Korean-English | 6.7 MB (dictzip) | 0 B (mmapped) |
| Spanish-English | 0 B (mmapped) | 0 B (mmapped) |

Lookup speed for Langdao/Korean will drop from ~60-80μs/word back to ~500ns/word.

Disk cost: 12.6 MB + 15.6 MB = 28.2 MB of new `.dict` files written next to
the existing `.dict.dz` files.

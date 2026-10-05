# Dictzip Random Access Plan

## Problem

Two of the four test dictionaries use `.dict.dz` (dictzip) and are fully
decompressed into memory at load time. This wastes 28.2 MB (12.6 + 15.6)
when the compressed files are only 10.5 MB (3.8 + 6.7) on disk.

## How dictzip works

Dictzip splits the input into fixed-size chunks (~58 KB) and compresses each
independently. A chunk index is stored in the gzip extra field header:

```
[standard gzip header, FLG has FEXTRA bit set]
  XLEN (2 bytes LE) — length of extra field
  Extra subfield:
    "RA"           (2 bytes) — subfield ID
    subfield_len   (2 bytes LE)
    version        (2 bytes LE) — always 1
    chunk_length   (2 bytes LE) — uncompressed bytes per chunk (58315)
    chunk_count    (2 bytes LE)
    chunk_sizes[]  (chunk_count × 2 bytes LE) — compressed size of each chunk
[optional FNAME, FCOMMENT, FHCRC per gzip spec]
[compressed chunk 0][compressed chunk 1]...[compressed chunk N]
[gzip trailer: CRC32 + ISIZE]
```

Each chunk is an independent deflate stream. To read bytes at offset `off`:
1. `chunk_idx = off / chunk_length`
2. Sum `chunk_sizes[0..chunk_idx]` to find where chunk starts in compressed data
3. Decompress just that one ~58 KB chunk
4. Slice out the needed bytes

## What changes

Only two files change: `src/dict.rs` and `examples/bench.rs`. No test changes
needed — the public API (`read_entry`, `data_len`) stays the same.

## File Changes

### 1. `src/dict.rs` — full replacement

```rust
use std::fs::{self, File};
use std::io::Read;
use std::path;

use anyhow::anyhow;
use flate2::read::GzDecoder;
use flate2::{Decompress, FlushDecompress};
use memmap2::Mmap;

#[derive(Debug, Clone)]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

struct DictZipIndex {
    raw: Vec<u8>,
    chunk_length: usize,
    chunk_sizes: Vec<u16>,
    chunk_offsets: Vec<u32>,
    data_start: usize,
}

enum DictData {
    Mapped(Mmap),
    Owned(Vec<u8>),
    DictZip(DictZipIndex),
}

pub struct Dict {
    data: DictData,
}

impl Dict {
    pub fn open(file: path::PathBuf) -> anyhow::Result<Dict> {
        let is_compressed = file.extension()
            .map_or(false, |ext| ext == "gz" || ext == "dz");
        if is_compressed {
            let raw = fs::read(&file)?;
            match Self::parse_dictzip(raw) {
                Ok(dz) => Ok(Dict { data: DictData::DictZip(dz) }),
                Err(_) => {
                    // Not valid dictzip — fall back to full decompression
                    let raw = fs::read(&file)?;
                    let mut decoder = GzDecoder::new(&raw[..]);
                    let mut decompressed = Vec::new();
                    decoder.read_to_end(&mut decompressed)?;
                    Ok(Dict { data: DictData::Owned(decompressed) })
                }
            }
        } else {
            let f = File::open(&file)?;
            // SAFETY: dictionary files are read-only; we do not modify them.
            let mmap = unsafe { Mmap::map(&f)? };
            Ok(Dict { data: DictData::Mapped(mmap) })
        }
    }

    fn parse_dictzip(data: Vec<u8>) -> anyhow::Result<DictZipIndex> {
        if data.len() < 12 || data[0] != 0x1f || data[1] != 0x8b {
            return Err(anyhow!("not a gzip file"));
        }
        if data[2] != 0x08 {
            return Err(anyhow!("not deflate"));
        }

        let flg = data[3];
        if flg & 0x04 == 0 {
            return Err(anyhow!("no FEXTRA — not dictzip"));
        }

        let mut pos = 10;

        // FEXTRA
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;
        let extra_end = pos + xlen;

        // Scan extra subfields for "RA"
        let mut chunk_length = 0usize;
        let mut chunk_sizes = Vec::new();
        let mut epos = pos;
        while epos + 4 <= extra_end {
            let si1 = data[epos];
            let si2 = data[epos + 1];
            let slen = u16::from_le_bytes([data[epos + 2], data[epos + 3]]) as usize;
            epos += 4;

            if si1 == b'R' && si2 == b'A' && slen >= 6 {
                let _version = u16::from_le_bytes([data[epos], data[epos + 1]]);
                chunk_length = u16::from_le_bytes([data[epos + 2], data[epos + 3]]) as usize;
                let chunk_count = u16::from_le_bytes([data[epos + 4], data[epos + 5]]) as usize;
                chunk_sizes.reserve(chunk_count);
                for i in 0..chunk_count {
                    let off = epos + 6 + i * 2;
                    chunk_sizes.push(u16::from_le_bytes([data[off], data[off + 1]]));
                }
            }
            epos += slen;
        }
        pos = extra_end;

        if chunk_sizes.is_empty() || chunk_length == 0 {
            return Err(anyhow!("no RA field found — not dictzip"));
        }

        // Skip optional FNAME
        if flg & 0x08 != 0 {
            while pos < data.len() && data[pos] != 0 { pos += 1; }
            pos += 1;
        }
        // Skip optional FCOMMENT
        if flg & 0x10 != 0 {
            while pos < data.len() && data[pos] != 0 { pos += 1; }
            pos += 1;
        }
        // Skip optional FHCRC
        if flg & 0x02 != 0 {
            pos += 2;
        }

        let data_start = pos;

        // Build cumulative offset table
        let mut chunk_offsets = Vec::with_capacity(chunk_sizes.len());
        let mut cumulative = 0u32;
        for &cs in &chunk_sizes {
            chunk_offsets.push(cumulative);
            cumulative += cs as u32;
        }

        Ok(DictZipIndex {
            raw: data,
            chunk_length,
            chunk_sizes,
            chunk_offsets,
            data_start,
        })
    }

    fn read_range(&self, offset: u64, size: u32) -> Vec<u8> {
        let start = offset as usize;
        let len = size as usize;
        match &self.data {
            DictData::Mapped(m) => m[start..start + len].to_vec(),
            DictData::Owned(v) => v[start..start + len].to_vec(),
            DictData::DictZip(dz) => {
                if len == 0 { return Vec::new(); }

                let first_chunk = start / dz.chunk_length;
                let last_chunk = (start + len - 1) / dz.chunk_length;
                let mut result = Vec::with_capacity(len);

                for ci in first_chunk..=last_chunk {
                    let comp_start = dz.data_start + dz.chunk_offsets[ci] as usize;
                    let comp_size = dz.chunk_sizes[ci] as usize;
                    let compressed = &dz.raw[comp_start..comp_start + comp_size];

                    let mut decompressor = Decompress::new(false);
                    let mut buf = vec![0u8; dz.chunk_length];
                    decompressor
                        .decompress(compressed, &mut buf, FlushDecompress::Finish)
                        .expect("dictzip chunk decompression failed");
                    let decomp_len = decompressor.total_out() as usize;

                    let chunk_file_start = ci * dz.chunk_length;
                    let local_start = start.saturating_sub(chunk_file_start);
                    let local_end = (start + len).saturating_sub(chunk_file_start).min(decomp_len);

                    result.extend_from_slice(&buf[local_start..local_end]);
                }

                result
            }
        }
    }

    pub fn data_len(&self) -> usize {
        match &self.data {
            DictData::Mapped(_) => 0,
            DictData::Owned(v) => v.len(),
            DictData::DictZip(dz) => {
                dz.raw.len() + dz.chunk_offsets.len() * 4 + dz.chunk_sizes.len() * 2
            }
        }
    }

    pub fn read_entry(
        &self,
        offset: u64,
        size: u32,
        sametypesequence: Option<&str>,
    ) -> Vec<DictEntry> {
        let range = self.read_range(offset, size);
        Self::parse_entries(&range, sametypesequence)
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

### 2. `examples/bench.rs` — update dict line to show storage type

Replace the dict memory line:

```diff
-    println!("    dict:  {:>10}", fmt_bytes(dict_total));
+    println!("    dict:  {:>10}  ({})", fmt_bytes(dict_total), dict.dict.storage_label());
```

And add this public method to `src/dict.rs`:

```rust
    pub fn storage_label(&self) -> &'static str {
        match &self.data {
            DictData::Mapped(_) => "mmapped",
            DictData::Owned(_) => "decompressed",
            DictData::DictZip(_) => "dictzip",
        }
    }
```

### 3. No other files change

- `src/dictionary.rs` — unchanged, calls `dict.read_entry()` as before
- All tests — unchanged, `read_entry()` and `data_len()` API is the same
- Other examples — unchanged

## Expected Results

| Dictionary | Dict before | Dict after | Total before | Total after |
|---|---|---|---|---|
| 现代汉语词��� | 5.6 MB (decompressed) | 0 B (mmapped) | 6.7 MB | 1.1 MB |
| 朗道汉英字典 | 12.6 MB (decompressed) | 3.8 MB (dictzip) | 22.6 MB | 14.0 MB |
| Korean-English | 15.6 MB (decompressed) | 6.7 MB (dictzip) | 16.6 MB | 7.7 MB |
| Spanish-English | 7.4 MB (decompressed) | 0 B (mmapped) | 28.7 MB | 21.3 MB |
| **Total** | **41.2 MB** | **10.5 MB** | **74.6 MB** | **~44 MB** |

Lookup will be slightly slower for dictzip dicts (~1-2 us per lookup for the
chunk decompression) but still fast enough for interactive use.

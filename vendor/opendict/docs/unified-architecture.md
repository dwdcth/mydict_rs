# Unified Architecture Plan

## Goal

A single `Dictionary` trait that works for both StarDict and MDict formats.
The consumer passes a directory path and a `DictKind` enum, and the library
figures out which files to load internally.

## New Directory Structure

```
src/
  lib.rs              # DictKind enum, Dictionary trait, open() dispatch
  types.rs            # DictEntry, DictInfo — shared types
  stardict/
    mod.rs            # StarDictDictionary implementing Dictionary trait
    ifo.rs            # (moved from src/ifo.rs)
    idx.rs            # (moved from src/idx.rs)
    dict.rs           # (moved from src/dict.rs)
    syn.rs            # (moved from src/syn.rs)
    strcmp.rs          # (moved from src/strcmp.rs)
    io.rs             # (moved from src/io.rs)
  mdict/
    mod.rs            # MdictDictionary implementing Dictionary trait
    header.rs         # UTF-16LE XML header parsing
    keys.rs           # keyword section: index + blocks
    records.rs        # record section: index + blocks
    decompress.rs     # compression dispatch (none/lzo/zlib)
```

## Shared Types — `src/types.rs`

```rust
#[derive(Debug, Clone)]
pub struct DictEntry {
    pub type_id: char,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DictInfo {
    pub name: String,
    pub author: String,
    pub description: String,
    pub word_count: u64,
}
```

## Dictionary Trait — `src/lib.rs`

```rust
pub mod types;
pub mod stardict;
pub mod mdict;

use std::path;
use types::{DictEntry, DictInfo};

pub enum DictKind {
    StarDict,
    MDict,
}

pub trait Dictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn lookup_synonym(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn word_list(&self) -> Vec<String>;
    fn word_count(&self) -> usize;
    fn info(&self) -> DictInfo;
}

pub fn open(dir: path::PathBuf, kind: DictKind) -> anyhow::Result<Box<dyn Dictionary>> {
    match kind {
        DictKind::StarDict => {
            let dict = stardict::StarDictDictionary::open(dir)?;
            Ok(Box::new(dict))
        }
        DictKind::MDict => {
            let dict = mdict::MdictDictionary::open(dir)?;
            Ok(Box::new(dict))
        }
    }
}
```

## StarDict Adapter — `src/stardict/mod.rs`

Wraps the existing code, implements the `Dictionary` trait. All existing
modules move under `src/stardict/` unchanged except import paths.

```rust
pub mod ifo;
pub mod idx;
pub mod dict;
pub mod syn;
pub mod strcmp;
pub mod io;

use std::path;
use anyhow::anyhow;

use crate::types::{DictEntry, DictInfo};
use crate::Dictionary;

pub struct StarDictDictionary {
    pub idx: idx::Idx,
    pub ifo: ifo::Ifo,
    pub dict: dict::Dict,
    pub syn: Option<syn::Syn>,
}

impl StarDictDictionary {
    pub fn open(dir: path::PathBuf) -> anyhow::Result<Self> {
        // Find the .ifo file in the directory
        let ifo_path = find_file_with_ext(&dir, "ifo")?;
        let name = ifo_path.file_stem().unwrap().to_str().unwrap();

        let ifo = ifo::Ifo::open(ifo_path)?;

        let idx_path = dir.join(format!("{}.idx", name));
        let idx_path = if idx_path.exists() {
            idx_path
        } else {
            dir.join(format!("{}.idx.gz", name))
        };
        let idx = idx::Idx::open(idx_path, ifo.word_count as u32, ifo.idx_offset_bits)?;

        let dict_path = dir.join(format!("{}.dict", name));
        let dict_path = if dict_path.exists() {
            dict_path
        } else {
            dir.join(format!("{}.dict.dz", name))
        };
        let dict = dict::Dict::open(dict_path)?;

        let syn_path = dir.join(format!("{}.syn", name));
        let syn = if syn_path.exists() && ifo.syn_word_count > 0 {
            Some(syn::Syn::open(syn_path, ifo.syn_word_count as u32)?)
        } else {
            None
        };

        Ok(StarDictDictionary { idx, ifo, dict, syn })
    }
}

impl Dictionary for StarDictDictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>> {
        let entry = self.idx.search(word)?;
        let sametypesequence = if self.ifo.same_type_sequence.is_empty() {
            None
        } else {
            Some(self.ifo.same_type_sequence.as_str())
        };
        Some(self.dict.read_entry(entry.offset, entry.size, sametypesequence))
    }

    fn lookup_synonym(&self, word: &str) -> Option<Vec<DictEntry>> {
        let syn = self.syn.as_ref()?;
        let syn_entry = syn.lookup(word)?;
        let i = syn_entry.original_word_index as usize;
        if i >= self.idx.entry_count() {
            return None;
        }
        let target = self.idx.entry(i);
        let sametypesequence = if self.ifo.same_type_sequence.is_empty() {
            None
        } else {
            Some(self.ifo.same_type_sequence.as_str())
        };
        Some(self.dict.read_entry(target.offset, target.size, sametypesequence))
    }

    fn word_list(&self) -> Vec<String> {
        (0..self.idx.entry_count())
            .map(|i| self.idx.word_at(i).to_string())
            .collect()
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
}

fn find_file_with_ext(dir: &path::Path, ext: &str) -> anyhow::Result<path::PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().map_or(false, |e| e == ext) {
            return Ok(path);
        }
    }
    Err(anyhow!("no .{} file found in {}", ext, dir.display()))
}
```

## MDict Stub — `src/mdict/mod.rs`

Initial stub that compiles. MDict internals built out separately.

```rust
pub mod header;
pub mod keys;
pub mod records;
pub mod decompress;

use std::path;
use anyhow::anyhow;

use crate::types::{DictEntry, DictInfo};
use crate::Dictionary;

pub struct MdictDictionary {
    info_data: DictInfo,
    keywords: Vec<String>,
    // Record offsets parallel to keywords — offset into virtual decompressed record stream
    record_offsets: Vec<u64>,
    // Raw file data for on-demand record block decompression
    data: Vec<u8>,
    // Record block index: (compressed_offset, compressed_size, decompressed_size)
    record_blocks: Vec<(u64, u64, u64)>,
    // Offset in file where record blocks start
    record_blocks_start: u64,
    // Format from header: 'h' for Html, 'm' for Text
    format_type: char,
}

impl MdictDictionary {
    pub fn open(dir: path::PathBuf) -> anyhow::Result<Self> {
        // Find .mdx file in the directory
        let mdx_path = find_mdx(&dir)?;
        let data = std::fs::read(&mdx_path)?;

        let header = header::parse_header(&data)?;

        let format_type = if header.format.eq_ignore_ascii_case("html") {
            'h'
        } else {
            'm'
        };

        let (keywords, record_offsets, key_end) = keys::parse_keywords(&data, &header)?;

        let (record_blocks, record_blocks_start) =
            records::parse_record_index(&data, key_end, &header)?;

        Ok(MdictDictionary {
            info_data: DictInfo {
                name: header.title,
                author: String::new(),
                description: header.description,
                word_count: keywords.len() as u64,
            },
            keywords,
            record_offsets,
            data,
            record_blocks,
            record_blocks_start,
            format_type,
        })
    }
}

impl Dictionary for MdictDictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>> {
        let i = self.keywords.binary_search_by(|k| k.as_str().cmp(word)).ok()?;
        let record_data = records::read_record(
            &self.data,
            self.record_blocks_start,
            &self.record_blocks,
            &self.record_offsets,
            i,
        ).ok()?;
        Some(vec![DictEntry {
            type_id: self.format_type,
            data: record_data,
        }])
    }

    fn lookup_synonym(&self, _word: &str) -> Option<Vec<DictEntry>> {
        None
    }

    fn word_list(&self) -> Vec<String> {
        self.keywords.clone()
    }

    fn word_count(&self) -> usize {
        self.keywords.len()
    }

    fn info(&self) -> DictInfo {
        self.info_data.clone()
    }
}

fn find_mdx(dir: &path::Path) -> anyhow::Result<path::PathBuf> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().map_or(false, |e| e.eq_ignore_ascii_case("mdx")) {
            return Ok(path);
        }
    }
    Err(anyhow!("no .mdx file found in {}", dir.display()))
}
```

## MDict Header — `src/mdict/header.rs`

```rust
use anyhow::anyhow;

pub struct MdictHeader {
    pub version: f32,
    pub encoding: String,
    pub format: String,
    pub title: String,
    pub description: String,
    pub encrypted: u8,
    pub key_case_sensitive: bool,
    // Byte offset where keyword section starts
    pub keyword_sect_start: usize,
}

pub fn parse_header(data: &[u8]) -> anyhow::Result<MdictHeader> {
    if data.len() < 8 {
        return Err(anyhow!("file too small"));
    }

    // Header length (4 bytes, big-endian)
    let header_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

    if data.len() < 4 + header_len + 4 {
        return Err(anyhow!("file truncated in header"));
    }

    // Header string is UTF-16LE
    let header_bytes = &data[4..4 + header_len];
    let header_str = decode_utf16le(header_bytes)?;

    // Skip checksum (4 bytes after header string)
    let keyword_sect_start = 4 + header_len + 4;

    // Parse XML attributes from the header string
    let mut version = 2.0f32;
    let mut encoding = "UTF-8".to_string();
    let mut format = "Html".to_string();
    let mut title = String::new();
    let mut description = String::new();
    let mut encrypted = 0u8;
    let mut key_case_sensitive = false;

    for (key, val) in parse_xml_attrs(&header_str) {
        match key.as_str() {
            "GeneratedByEngineVersion" => {
                version = val.parse().unwrap_or(2.0);
            }
            "Encoding" => encoding = val,
            "Format" => format = val,
            "Title" => title = val,
            "Description" => description = val,
            "Encrypted" => encrypted = val.parse().unwrap_or(0),
            "KeyCaseSensitive" => {
                key_case_sensitive = val.eq_ignore_ascii_case("yes");
            }
            _ => {}
        }
    }

    Ok(MdictHeader {
        version,
        encoding,
        format,
        title,
        description,
        encrypted,
        key_case_sensitive,
        keyword_sect_start,
    })
}

fn decode_utf16le(data: &[u8]) -> anyhow::Result<String> {
    if data.len() % 2 != 0 {
        return Err(anyhow!("odd byte count for UTF-16LE"));
    }
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&u16s).map_err(|e| anyhow!("invalid UTF-16LE: {}", e))
}

fn parse_xml_attrs(xml: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    // Simple attribute parser: key="value" or key='value'
    let mut remaining = xml;
    while let Some(eq_pos) = remaining.find('=') {
        // Extract key (last word before '=')
        let before_eq = &remaining[..eq_pos];
        let key = before_eq
            .rsplit(|c: char| c.is_whitespace() || c == '<' || c == '/')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        remaining = &remaining[eq_pos + 1..];
        let remaining_trimmed = remaining.trim_start();

        // Find quoted value
        if let Some(quote) = remaining_trimmed.chars().next() {
            if quote == '"' || quote == '\'' {
                let after_open = &remaining_trimmed[1..];
                if let Some(close) = after_open.find(quote) {
                    let val = after_open[..close].to_string();
                    if !key.is_empty() {
                        attrs.push((key, val));
                    }
                    remaining = &after_open[close + 1..];
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            break;
        }
    }
    attrs
}
```

## MDict Decompression — `src/mdict/decompress.rs`

```rust
use anyhow::anyhow;
use flate2::read::ZlibDecoder;
use std::io::Read;

/// Decompress an MDict data block.
/// Format: 4 bytes comp_type + 4 bytes checksum + compressed_data
pub fn decompress_block(block: &[u8]) -> anyhow::Result<Vec<u8>> {
    if block.len() < 8 {
        return Err(anyhow!("block too small"));
    }

    let comp_type = &block[0..4];
    // checksum at block[4..8]
    let compressed = &block[8..];

    match comp_type {
        [0, 0, 0, 0] => {
            // No compression
            Ok(compressed.to_vec())
        }
        [1, 0, 0, 0] => {
            // LZO — requires lzo crate or minilzo
            Err(anyhow!("LZO compression not yet supported"))
        }
        [2, 0, 0, 0] => {
            // zlib
            let mut decoder = ZlibDecoder::new(compressed);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            Ok(decompressed)
        }
        _ => Err(anyhow!("unknown compression type: {:?}", comp_type)),
    }
}
```

## MDict Keywords — `src/mdict/keys.rs`

```rust
use anyhow::anyhow;
use crate::mdict::header::MdictHeader;
use crate::mdict::decompress;

/// Parse keyword section. Returns (keywords, record_offsets, end_position).
pub fn parse_keywords(
    data: &[u8],
    header: &MdictHeader,
) -> anyhow::Result<(Vec<String>, Vec<u64>, usize)> {
    let mut pos = header.keyword_sect_start;

    if header.encrypted & 1 != 0 {
        return Err(anyhow!("encrypted keyword header not yet supported"));
    }

    // Keyword section header (v2): 5 × 8 bytes + 4 byte checksum = 44 bytes
    if pos + 44 > data.len() {
        return Err(anyhow!("keyword section header truncated"));
    }

    let num_blocks = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _num_entries = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _key_index_decomp_len = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let key_index_comp_len = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let key_blocks_len = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _checksum = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;

    // Keyword index (compressed) — skip it, we'll parse blocks directly
    if header.encrypted & 2 != 0 {
        return Err(anyhow!("encrypted keyword index not yet supported"));
    }

    let key_index_end = pos + key_index_comp_len as usize;

    // Parse the key index to get block sizes
    let key_index_data = decompress::decompress_block(&data[pos..key_index_end])?;
    let block_infos = parse_key_index(&key_index_data, num_blocks as usize, header)?;

    pos = key_index_end;

    // Parse keyword blocks
    let mut keywords = Vec::new();
    let mut record_offsets = Vec::new();

    for (comp_size, _decomp_size) in &block_infos {
        let block_end = pos + *comp_size as usize;
        let block_data = decompress::decompress_block(&data[pos..block_end])?;
        parse_key_block(&block_data, header, &mut keywords, &mut record_offsets)?;
        pos = block_end;
    }

    debug_assert_eq!(pos, key_index_end + key_blocks_len as usize);

    Ok((keywords, record_offsets, pos))
}

/// Parse the decompressed key index to extract (comp_size, decomp_size) per block.
fn parse_key_index(
    data: &[u8],
    num_blocks: usize,
    header: &MdictHeader,
) -> anyhow::Result<Vec<(u64, u64)>> {
    let mut pos = 0;
    let mut blocks = Vec::with_capacity(num_blocks);
    let encoding_unit = if header.encoding.eq_ignore_ascii_case("utf-16")
        || header.encoding.eq_ignore_ascii_case("utf-16le")
    {
        2usize
    } else {
        1usize
    };

    for _ in 0..num_blocks {
        if pos + 8 > data.len() {
            return Err(anyhow!("key index truncated"));
        }
        // num_entries for this block
        let _num_entries = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // first_word: 2-byte length + word bytes + null terminator
        let first_len = u16::from_be_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let first_bytes = first_len * encoding_unit + encoding_unit; // include null
        pos += first_bytes;

        // last_word: 2-byte length + word bytes + null terminator
        let last_len = u16::from_be_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let last_bytes = last_len * encoding_unit + encoding_unit;
        pos += last_bytes;

        // comp_size, decomp_size
        let comp_size = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let decomp_size = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        blocks.push((comp_size, decomp_size));
    }

    Ok(blocks)
}

/// Parse a decompressed key block into keywords and record offsets.
fn parse_key_block(
    data: &[u8],
    header: &MdictHeader,
    keywords: &mut Vec<String>,
    record_offsets: &mut Vec<u64>,
) -> anyhow::Result<()> {
    let mut pos = 0;
    let is_utf16 = header.encoding.eq_ignore_ascii_case("utf-16")
        || header.encoding.eq_ignore_ascii_case("utf-16le");

    while pos < data.len() {
        if pos + 8 > data.len() {
            break;
        }

        // Record offset (8 bytes, big-endian)
        let offset = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        record_offsets.push(offset);

        // Null-terminated keyword
        if is_utf16 {
            let start = pos;
            while pos + 1 < data.len() && !(data[pos] == 0 && data[pos + 1] == 0) {
                pos += 2;
            }
            let u16s: Vec<u16> = data[start..pos]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            keywords.push(String::from_utf16_lossy(&u16s));
            pos += 2; // skip null terminator
        } else {
            let start = pos;
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            let word = String::from_utf8_lossy(&data[start..pos]).into_owned();
            keywords.push(word);
            pos += 1; // skip null terminator
        }
    }

    Ok(())
}
```

## MDict Records — `src/mdict/records.rs`

```rust
use anyhow::anyhow;
use crate::mdict::header::MdictHeader;
use crate::mdict::decompress;

/// Parse record section index. Returns (block_infos, record_blocks_start_offset).
/// Each block_info is (compressed_offset_in_file, compressed_size, decompressed_size).
pub fn parse_record_index(
    data: &[u8],
    start: usize,
    _header: &MdictHeader,
) -> anyhow::Result<(Vec<(u64, u64, u64)>, u64)> {
    let mut pos = start;

    if pos + 32 > data.len() {
        return Err(anyhow!("record section header truncated"));
    }

    let num_blocks = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _num_entries = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _index_len = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;
    let _blocks_len = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
    pos += 8;

    // Read block size pairs
    let mut blocks = Vec::with_capacity(num_blocks as usize);
    let mut cumulative_offset = 0u64;

    let index_start = pos;
    for _ in 0..num_blocks {
        if pos + 16 > data.len() {
            return Err(anyhow!("record index truncated"));
        }
        let comp_size = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let decomp_size = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        blocks.push((cumulative_offset, comp_size, decomp_size));
        cumulative_offset += comp_size;
    }

    let record_blocks_start = pos as u64;

    Ok((blocks, record_blocks_start))
}

/// Read a single record by keyword index.
pub fn read_record(
    data: &[u8],
    record_blocks_start: u64,
    record_blocks: &[(u64, u64, u64)],
    record_offsets: &[u64],
    keyword_index: usize,
) -> anyhow::Result<Vec<u8>> {
    let offset = record_offsets[keyword_index];

    // Figure out how long this record is
    let record_end = if keyword_index + 1 < record_offsets.len() {
        record_offsets[keyword_index + 1]
    } else {
        // Last entry — extends to end of last decompressed block
        record_blocks.iter().map(|(_, _, d)| d).sum::<u64>()
    };
    let record_len = (record_end - offset) as usize;

    // Find which block(s) this record falls in
    let mut decompressed_pos = 0u64;
    let mut result = Vec::with_capacity(record_len);

    for &(comp_offset, comp_size, decomp_size) in record_blocks {
        let block_start = decompressed_pos;
        let block_end = decompressed_pos + decomp_size;

        if offset < block_end && (offset + record_len as u64) > block_start {
            // This block overlaps with our record
            let file_offset = record_blocks_start + comp_offset;
            let block_data = &data[file_offset as usize..(file_offset + comp_size) as usize];
            let decompressed = decompress::decompress_block(block_data)?;

            let local_start = offset.saturating_sub(block_start) as usize;
            let local_end = ((offset + record_len as u64) - block_start)
                .min(decomp_size) as usize;

            result.extend_from_slice(&decompressed[local_start..local_end]);

            if result.len() >= record_len {
                break;
            }
        }

        decompressed_pos = block_end;
    }

    // Strip null terminator if present
    if result.last() == Some(&0) {
        result.pop();
    }

    Ok(result)
}
```

## Changes to Node/Mobile Bindings

Both `node/src/lib.rs` and `mobile/src/lib.rs` update to use the trait:

### `node/src/lib.rs`

```rust
use napi::bindgen_prelude::*;
use napi_derive::napi;

use std::path::PathBuf;

#[napi(object)]
pub struct DictEntry {
    pub entry_type: String,
    pub data: String,
}

#[napi(object)]
pub struct DictInfo {
    pub name: String,
    pub author: String,
    pub description: String,
    pub word_count: u32,
}

#[napi]
pub struct Dictionary {
    inner: Box<dyn stardict::Dictionary>,
}

#[napi]
impl Dictionary {
    #[napi(constructor)]
    pub fn new(dir: String, kind: String) -> Result<Self> {
        let dict_kind = match kind.as_str() {
            "stardict" => stardict::DictKind::StarDict,
            "mdict" => stardict::DictKind::MDict,
            _ => return Err(Error::from_reason(format!("unknown dict kind: {}", kind))),
        };
        let inner = stardict::open(PathBuf::from(&dir), dict_kind)
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(Dictionary { inner })
    }

    #[napi]
    pub fn lookup(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
    }

    #[napi]
    pub fn lookup_synonym(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup_synonym(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
    }

    #[napi]
    pub fn word_list(&self) -> Vec<String> {
        self.inner.word_list()
    }

    #[napi]
    pub fn word_count(&self) -> u32 {
        self.inner.word_count() as u32
    }

    #[napi(getter)]
    pub fn name(&self) -> String {
        self.inner.info().name
    }

    #[napi(getter)]
    pub fn author(&self) -> String {
        self.inner.info().author
    }

    #[napi(getter)]
    pub fn description(&self) -> String {
        self.inner.info().description
    }
}
```

### `mobile/src/lib.rs`

Same pattern — change `inner` from `stardict::dictionary::Dictionary` to
`Box<dyn stardict::Dictionary>`, add `kind` parameter to constructor.

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

#[derive(uniffi::Object)]
pub struct Dictionary {
    inner: Box<dyn stardict::Dictionary + Send + Sync>,
}

#[uniffi::export]
impl Dictionary {
    #[uniffi::constructor]
    fn new(dir: String, kind: DictKind) -> Result<Arc<Self>, DictError> {
        let k = match kind {
            DictKind::StarDict => stardict::DictKind::StarDict,
            DictKind::MDict => stardict::DictKind::MDict,
        };
        let inner = stardict::open(PathBuf::from(&dir), k)
            .map_err(|e| DictError::LoadError { message: e.to_string() })?;
        Ok(Arc::new(Dictionary { inner }))
    }

    fn lookup(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
    }

    fn lookup_synonym(&self, word: String) -> Option<Vec<DictEntry>> {
        self.inner.lookup_synonym(&word).map(|entries| {
            entries
                .into_iter()
                .map(|e| DictEntry {
                    entry_type: e.type_id.to_string(),
                    data: String::from_utf8_lossy(&e.data).into_owned(),
                })
                .collect()
        })
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
            name: i.name,
            author: i.author,
            description: i.description,
            word_count: i.word_count,
        }
    }
}

#[derive(Debug, uniffi::Error)]
pub enum DictError {
    LoadError { message: String },
}

impl std::fmt::Display for DictError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DictError::LoadError { message } => write!(f, "{}", message),
        }
    }
}
```

## Changes to Tests

Existing StarDict tests continue to work — they import from
`stardict::stardict::idx`, `stardict::stardict::ifo`, etc. The test import
paths need updating but the test logic is unchanged.

Alternatively, re-export the stardict submodules from `lib.rs`:
```rust
// in src/lib.rs — backwards compatibility
pub use stardict::dict;
pub use stardict::idx;
pub use stardict::ifo;
pub use stardict::syn;
pub use stardict::strcmp;
pub use stardict::io;
pub use stardict::StarDictDictionary as dictionary;
```

## Changes to `Cargo.toml`

No new dependencies for the initial implementation (zlib decompression already
available via `flate2`). LZO support can be added later with `minilzo-rs` or
`lzo1x-1` crate.

## Migration Steps

1. Create `src/types.rs` with shared types
2. Create `src/stardict/` directory, move existing files into it
3. Create `src/stardict/mod.rs` with `StarDictDictionary`
4. Create `src/mdict/` with stub files
5. Rewrite `src/lib.rs` with trait + `DictKind` + `open()`
6. Add re-exports for backwards compatibility with tests
7. Update test import paths if needed
8. Update `node/src/lib.rs` and `mobile/src/lib.rs`
9. Verify all 87 tests still pass
10. Build and test MDict with a real `.mdx` file

## Usage After

```rust
use stardict::{open, DictKind};

let sd = open("/path/to/stardict-dir".into(), DictKind::StarDict)?;
let md = open("/path/to/mdict-dir".into(), DictKind::MDict)?;

// Same interface
sd.lookup("hello");
md.lookup("hello");
```

```javascript
// Node
const sd = new Dictionary('/path/to/stardict-dir', 'stardict')
const md = new Dictionary('/path/to/mdict-dir', 'mdict')
sd.lookup('hello')
md.lookup('hello')
```

```typescript
// Expo
const sd = new Dictionary('/path/to/stardict-dir', DictKind.StarDict)
const md = new Dictionary('/path/to/mdict-dir', DictKind.MDict)
```

# MDict Priority 1 Implementation Plan

Five features, in implementation order (dependencies flow downward):

1. **GBK/Big5 encoding** — unblocks Chinese dicts
2. **Adler32 checksum verification** — data integrity
3. **v3.0 encrypted file support** — completes format coverage
4. **MDD resource files** — CSS/images for HTML dicts
5. **Prefix search** — autocomplete for both StarDict and MDict

---

## 1. GBK/Big5 Encoding Support

### Cargo.toml

```toml
[dependencies]
encoding_rs = "0.8"
```

### New: `src/mdict/encoding.rs`

Centralises all byte-to-string decoding. Used by keyword parsing and record decoding.

```rust
use encoding_rs::Encoding;

/// Decode bytes using the encoding name from the MDict header.
/// Returns a UTF-8 String regardless of source encoding.
pub fn decode_str(bytes: &[u8], encoding_name: &str) -> String {
    match encoding_name.to_uppercase().as_str() {
        "UTF-8" | "UTF8" => String::from_utf8_lossy(bytes).into_owned(),
        "UTF-16" | "UTF-16LE" | "UTF16" => {
            let u16s: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&u16s)
        }
        label => {
            if let Some(enc) = Encoding::for_label(label.as_bytes()) {
                let (decoded, _, _) = enc.decode_without_bom_handling(bytes);
                decoded.into_owned()
            } else {
                // Unknown encoding — fallback to lossy UTF-8
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}
```

### Modify: `src/mdict/keys.rs` — `parse_key_block()`

Replace the current UTF-8 / UTF-16 branching with the unified decoder.

```rust
// BEFORE (lines 135-169):
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
        if pos + 8 > data.len() { break; }
        let offset = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        record_offsets.push(offset);
        if is_utf16 {
            // ... UTF-16 null scan
        } else {
            // ... UTF-8 null scan
        }
    }
    Ok(())
}

// AFTER:
fn parse_key_block(
    data: &[u8],
    header: &MdictHeader,
    keywords: &mut Vec<String>,
    record_offsets: &mut Vec<u64>,
) -> anyhow::Result<()> {
    let mut pos = 0;
    let null_width = encoding_null_width(&header.encoding);

    while pos < data.len() {
        if pos + 8 > data.len() { break; }
        let offset = u64::from_be_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        record_offsets.push(offset);

        // Find null terminator (1 byte for single-byte encodings, 2 bytes for UTF-16)
        let start = pos;
        if null_width == 2 {
            while pos + 1 < data.len() && !(data[pos] == 0 && data[pos + 1] == 0) {
                pos += 2;
            }
        } else {
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
        }
        keywords.push(super::encoding::decode_str(&data[start..pos], &header.encoding));
        pos += null_width; // skip null terminator
    }
    Ok(())
}

/// Returns the null-terminator width for the encoding: 2 for UTF-16, 1 for everything else.
fn encoding_null_width(encoding: &str) -> usize {
    let upper = encoding.to_uppercase();
    if upper == "UTF-16" || upper == "UTF-16LE" || upper == "UTF16" {
        2
    } else {
        1
    }
}
```

### Modify: `src/mdict/mod.rs` — store encoding, decode records

Add `encoding` field to `MdictDictionary`. Decode MDX record bytes to UTF-8 at lookup time.

```rust
// In MdictDictionary struct, add:
    encoding: String,

// In open(), store it:
    let encoding = header.encoding.clone();
    // ... later:
    Ok(MdictDictionary {
        // ... existing fields ...
        encoding,
    })

// In lookup(), decode the record data:
fn lookup(&self, word: &str) -> Option<Vec<DictEntry>> {
    let lookup_key = if self.case_sensitive {
        word.to_string()
    } else {
        word.to_lowercase()
    };
    let &i = self.keyword_map.get(&lookup_key)?;
    let record_data = records::read_record(
        &self.data,
        self.record_blocks_start,
        &self.record_blocks,
        &self.record_offsets,
        i,
    ).ok()?;

    // Decode record bytes from source encoding to UTF-8
    let decoded = encoding::decode_str(&record_data, &self.encoding);

    Some(vec![DictEntry {
        type_id: self.format_type,
        data: decoded.into_bytes(),
    }])
}
```

### Modify: `src/mdict/mod.rs` — register new module

```rust
pub mod encoding;
```

---

## 2. Adler32 Checksum Verification

### Cargo.toml

```toml
[dependencies]
adler2 = "2"
```

### Modify: `src/mdict/decompress.rs`

Add checksum verification after decompression/decryption. The version determines when to verify:
- v2: checksum is of **decompressed** data
- v3: checksum is of **decrypted** (pre-decompression) data

```rust
use adler2::adler32_slice;

/// Decompress an MDict data block with optional checksum verification.
/// `version` controls verification order (v2 vs v3).
pub fn decompress_block(block: &[u8], version: f32) -> anyhow::Result<Vec<u8>> {
    if block.len() < 8 {
        return Err(anyhow!("block too small"));
    }

    let info = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    let compression_method = info & 0xf;
    let encryption_method = (info >> 4) & 0xf;
    let encryption_size = ((info >> 8) & 0xff) as usize;

    let stored_checksum = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
    let mut data = block[8..].to_vec();

    // Handle per-block encryption
    if encryption_method == 1 {
        let key = super::ripemd128::ripemd128(&block[4..8]);
        let n = if encryption_size > 0 {
            encryption_size.min(data.len())
        } else {
            data.len()
        };
        let decrypted = super::decrypt::fast_decrypt(&data[..n], &key);
        data[..n].copy_from_slice(&decrypted);
    } else if encryption_method != 0 {
        return Err(anyhow!(
            "unsupported per-block encryption method: {}",
            encryption_method
        ));
    }

    // v3+: verify checksum on decrypted data (before decompression)
    if version >= 3.0 {
        let computed = adler32_slice(&data);
        if computed != stored_checksum {
            return Err(anyhow!(
                "adler32 mismatch (v3 decrypted): stored={:#010x} computed={:#010x}",
                stored_checksum, computed
            ));
        }
    }

    let decompressed = match compression_method {
        0 => data,
        1 => return Err(anyhow!("LZO compression not yet supported")),
        2 => {
            let mut decoder = flate2::read::ZlibDecoder::new(&data[..]);
            let mut buf = Vec::new();
            decoder.read_to_end(&mut buf)?;
            buf
        }
        _ => return Err(anyhow!("unknown compression type: {}", compression_method)),
    };

    // v2: verify checksum on decompressed data
    if version < 3.0 {
        let computed = adler32_slice(&decompressed);
        if computed != stored_checksum {
            return Err(anyhow!(
                "adler32 mismatch (v2 decompressed): stored={:#010x} computed={:#010x}",
                stored_checksum, computed
            ));
        }
    }

    Ok(decompressed)
}
```

### Modify: `src/mdict/keys.rs`

Thread `version` through to `decompress_block` calls, and verify the keyword section header checksum.

```rust
pub fn parse_keywords(
    data: &[u8],
    header: &MdictHeader,
) -> anyhow::Result<(Vec<String>, Vec<u64>, usize)> {
    let mut pos = header.keyword_sect_start;

    if header.encrypted & 1 != 0 {
        return Err(anyhow!("encrypted keyword header not yet supported"));
    }

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
    let stored_checksum = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
    pos += 4;

    // Verify keyword section header checksum (adler32 of the preceding 40 bytes)
    let header_bytes = &data[header.keyword_sect_start..header.keyword_sect_start + 40];
    let computed = adler2::adler32_slice(header_bytes);
    if computed != stored_checksum {
        return Err(anyhow!(
            "keyword section header checksum mismatch: stored={:#010x} computed={:#010x}",
            stored_checksum, computed
        ));
    }

    // ... rest unchanged, but pass header.version to decompress_block:
    let key_index_data = decompress::decompress_block(&key_index_block, header.version)?;

    // ... and for keyword blocks:
    let block_data = decompress::decompress_block(&data[pos..block_end], header.version)?;
```

### Modify: `src/mdict/records.rs`

Thread `version` to `decompress_block`:

```rust
pub fn read_record(
    data: &[u8],
    record_blocks_start: u64,
    record_blocks: &[(u64, u64, u64)],
    record_offsets: &[u64],
    keyword_index: usize,
    version: f32,
) -> anyhow::Result<Vec<u8>> {
    // ... existing code, but change the decompress call:
    let decompressed = decompress::decompress_block(block_data, version)?;
```

### Modify: `src/mdict/mod.rs`

Store `version` and pass it through to `read_record`:

```rust
// In struct:
    version: f32,

// In open():
    let version = header.version;

// In lookup():
    let record_data = records::read_record(
        &self.data,
        self.record_blocks_start,
        &self.record_blocks,
        &self.record_offsets,
        i,
        self.version,
    ).ok()?;
```

---

## 3. v3.0 Encrypted File Support

### Cargo.toml

```toml
[dependencies.xxhash-rust]
version = "0.8"
features = ["xxh64"]
```

### Modify: `src/mdict/header.rs`

Add `uuid` field to `MdictHeader`. Parse it from header XML.

```rust
pub struct MdictHeader {
    pub version: f32,
    pub encoding: String,
    pub format: String,
    pub title: String,
    pub description: String,
    pub encrypted: u8,
    pub key_case_sensitive: bool,
    pub keyword_sect_start: usize,
    pub uuid: Option<Vec<u8>>,  // NEW: raw UUID bytes for v3 key derivation
}

// In parse_header(), add to the match:
    "UUID" => uuid = Some(val.into_bytes()),

// And in the Ok(...):
    uuid,
```

### New: `src/mdict/keygen.rs`

Key derivation for all MDict versions.

```rust
use super::ripemd128::ripemd128;

/// Derive the encryption key for a given MDict file.
/// - v2: no global key (per-block key derived from block checksum)
/// - v3: key derived from UUID via xxhash64
pub fn derive_key(version: f32, uuid: Option<&[u8]>) -> Option<[u8; 16]> {
    if version >= 3.0 {
        let uuid = uuid?;
        let mid = (uuid.len() + 1) / 2;
        let h1 = xxhash_rust::xxh64::xxh64(&uuid[..mid], 0);
        let h2 = xxhash_rust::xxh64::xxh64(&uuid[mid..], 0);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&h1.to_le_bytes());
        key[8..].copy_from_slice(&h2.to_le_bytes());
        Some(key)
    } else {
        None
    }
}
```

### Modify: `src/mdict/decompress.rs`

Accept an optional global encryption key. If provided, use it instead of deriving per-block.

```rust
/// Decompress a block. If `global_key` is Some, use it for per-block encryption
/// instead of deriving from the block checksum.
pub fn decompress_block(
    block: &[u8],
    version: f32,
    global_key: Option<&[u8; 16]>,
) -> anyhow::Result<Vec<u8>> {
    // ...
    if encryption_method == 1 {
        let key = match global_key {
            Some(k) => *k,
            None => super::ripemd128::ripemd128(&block[4..8]),
        };
        // ... rest same
    }
    // ...
}
```

### Modify: `src/mdict/keys.rs`

For keyword index decryption, v3 encrypted files use the UUID-derived key instead of `ripemd128(checksum + magic)`.

```rust
pub fn parse_keywords(
    data: &[u8],
    header: &MdictHeader,
    global_key: Option<&[u8; 16]>,
) -> anyhow::Result<(Vec<String>, Vec<u64>, usize)> {
    // ...

    // Keyword index decryption
    let key_index_block = if header.encrypted & 2 != 0 {
        let block = &data[pos..key_index_end];
        if block.len() < 8 {
            return Err(anyhow!("keyword index block too small to decrypt"));
        }
        let key = match global_key {
            Some(k) => *k,
            None => {
                // v2 key derivation: ripemd128(checksum + magic)
                let mut key_input = [0u8; 8];
                key_input[..4].copy_from_slice(&block[4..8]);
                key_input[4..].copy_from_slice(&[0x95, 0x36, 0x00, 0x00]);
                super::ripemd128::ripemd128(&key_input)
            }
        };
        let mut decrypted = Vec::with_capacity(block.len());
        decrypted.extend_from_slice(&block[..8]);
        decrypted.extend_from_slice(&super::decrypt::fast_decrypt(&block[8..], &key));
        decrypted
    } else {
        data[pos..key_index_end].to_vec()
    };

    // Pass global_key and version through to decompress_block
    let key_index_data = decompress::decompress_block(&key_index_block, header.version, global_key)?;
    // ...
    // Same for keyword blocks:
    let block_data = decompress::decompress_block(&data[pos..block_end], header.version, global_key)?;
```

### Modify: `src/mdict/mod.rs`

Derive key during open, store it, pass through everywhere.

```rust
// In struct:
    global_key: Option<[u8; 16]>,

// In open():
    let global_key = keygen::derive_key(header.version, header.uuid.as_deref());

    let (keywords, record_offsets, key_end) =
        keys::parse_keywords(&data, &header, global_key.as_ref())?;

// In lookup(), pass to read_record:
    let record_data = records::read_record(
        &self.data,
        self.record_blocks_start,
        &self.record_blocks,
        &self.record_offsets,
        i,
        self.version,
        self.global_key.as_ref(),
    ).ok()?;
```

### Modify: `src/mdict/records.rs`

Thread `global_key` to `decompress_block`:

```rust
pub fn read_record(
    data: &[u8],
    record_blocks_start: u64,
    record_blocks: &[(u64, u64, u64)],
    record_offsets: &[u64],
    keyword_index: usize,
    version: f32,
    global_key: Option<&[u8; 16]>,
) -> anyhow::Result<Vec<u8>> {
    // ...
    let decompressed = decompress::decompress_block(block_data, version, global_key)?;
```

### Modify: `src/mdict/mod.rs` — register module

```rust
pub mod keygen;
```

---

## 4. MDD Resource File Support

### New: `src/mdict/file.rs`

Extract shared MDX/MDD parsing into a reusable struct.

```rust
use std::collections::HashMap;
use super::{header, keys, records, decompress};
use super::header::MdictHeader;

/// Parsed MDict file (either .mdx or .mdd).
pub struct MdictFile {
    pub header: MdictHeader,
    pub keywords: Vec<String>,
    pub keyword_map: HashMap<String, usize>,
    pub record_offsets: Vec<u64>,
    pub data: Vec<u8>,
    pub record_blocks: Vec<(u64, u64, u64)>,
    pub record_blocks_start: u64,
    pub global_key: Option<[u8; 16]>,
}

impl MdictFile {
    pub fn open(path: &std::path::Path, case_sensitive: Option<bool>) -> anyhow::Result<Self> {
        let data = std::fs::read(path)?;
        let header = header::parse_header(&data)?;
        let global_key = super::keygen::derive_key(header.version, header.uuid.as_deref());

        let (keywords, record_offsets, key_end) =
            keys::parse_keywords(&data, &header, global_key.as_ref())?;

        let (record_blocks, record_blocks_start) =
            records::parse_record_index(&data, key_end, &header)?;

        // Use provided case_sensitive or fall back to header value
        let case_sensitive = case_sensitive.unwrap_or(header.key_case_sensitive);

        let keyword_map: HashMap<String, usize> = keywords
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let key = if case_sensitive { k.clone() } else { k.to_lowercase() };
                (key, i)
            })
            .collect();

        Ok(MdictFile {
            header,
            keywords,
            keyword_map,
            record_offsets,
            data,
            record_blocks,
            record_blocks_start,
            global_key,
        })
    }

    /// Look up a keyword and return the raw record bytes.
    pub fn lookup_raw(&self, key: &str) -> Option<Vec<u8>> {
        let &i = self.keyword_map.get(key)?;
        records::read_record(
            &self.data,
            self.record_blocks_start,
            &self.record_blocks,
            &self.record_offsets,
            i,
            self.header.version,
            self.global_key.as_ref(),
        ).ok()
    }
}
```

### Modify: `src/mdict/mod.rs`

Refactor MdictDictionary to use MdictFile internally, and load .mdd files alongside .mdx.

```rust
pub mod file;

pub struct MdictDictionary {
    info_data: DictInfo,
    mdx: file::MdictFile,
    mdd: Vec<file::MdictFile>,
    format_type: char,
    case_sensitive: bool,
    encoding: String,
}

impl MdictDictionary {
    pub fn open(dir: path::PathBuf) -> anyhow::Result<Self> {
        let mdx_path = find_mdx(&dir)?;
        let mdx = file::MdictFile::open(&mdx_path, None)?;

        let format_type = if mdx.header.format.eq_ignore_ascii_case("html") { 'h' } else { 'm' };
        let case_sensitive = mdx.header.key_case_sensitive;
        let encoding = mdx.header.encoding.clone();

        let info_data = DictInfo {
            name: mdx.header.title.clone(),
            author: String::new(),
            description: mdx.header.description.clone(),
            word_count: mdx.keywords.len() as u64,
        };

        // Load .mdd files (same name as .mdx, plus numbered: .1.mdd, .2.mdd, ...)
        let mdd = load_mdd_files(&mdx_path)?;

        Ok(MdictDictionary {
            info_data,
            mdx,
            mdd,
            format_type,
            case_sensitive,
            encoding,
        })
    }

    /// Look up a resource (CSS, image, font, etc.) from .mdd files.
    /// Path should match the MDD keyword format, e.g. `\style.css` or `/style.css`.
    pub fn lookup_resource(&self, path: &str) -> Option<Vec<u8>> {
        // Normalise path separators (MDD uses backslash)
        let normalised = path.replace('/', "\\");
        let lookup_key = normalised.to_lowercase(); // MDD keys are case-insensitive
        for mdd in &self.mdd {
            if let Some(data) = mdd.lookup_raw(&lookup_key) {
                return Some(data);
            }
        }
        None
    }
}

impl Dictionary for MdictDictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>> {
        let lookup_key = if self.case_sensitive {
            word.to_string()
        } else {
            word.to_lowercase()
        };
        let record_data = self.mdx.lookup_raw(&lookup_key)?;

        // Decode from source encoding to UTF-8
        let decoded = encoding::decode_str(&record_data, &self.encoding);

        Some(vec![DictEntry {
            type_id: self.format_type,
            data: decoded.into_bytes(),
        }])
    }

    fn lookup_synonym(&self, _word: &str) -> Option<Vec<DictEntry>> { None }

    fn word_list(&self) -> Vec<String> {
        self.mdx.keywords.clone()
    }

    fn word_count(&self) -> usize {
        self.mdx.keywords.len()
    }

    fn info(&self) -> DictInfo {
        self.info_data.clone()
    }
}

/// Find .mdd files alongside the .mdx file.
/// Looks for: same_name.mdd, same_name.1.mdd, same_name.2.mdd, ...
fn load_mdd_files(mdx_path: &std::path::Path) -> anyhow::Result<Vec<file::MdictFile>> {
    let stem = mdx_path.file_stem().unwrap().to_string_lossy();
    let dir = mdx_path.parent().unwrap();
    let mut mdds = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let fname = path.file_name().unwrap().to_string_lossy().to_string();
        let fname_lower = fname.to_lowercase();
        if fname_lower.ends_with(".mdd") && fname_lower.starts_with(&stem.to_lowercase()) {
            match file::MdictFile::open(&path, Some(false)) {
                Ok(mdd) => mdds.push(mdd),
                Err(e) => eprintln!("Warning: failed to load MDD {}: {}", path.display(), e),
            }
        }
    }

    Ok(mdds)
}
```

### Modify: `src/mdict/mod.rs` — register module

```rust
pub mod file;
```

---

## 5. Prefix Search

### Modify: `src/lib.rs` — add to Dictionary trait

```rust
pub trait Dictionary {
    fn lookup(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn lookup_synonym(&self, word: &str) -> Option<Vec<DictEntry>>;
    fn word_list(&self) -> Vec<String>;
    fn word_count(&self) -> usize;
    fn info(&self) -> DictInfo;
    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String>;
}
```

### Modify: `src/stardict/mod.rs` — StarDict prefix search

StarDict idx is already sorted by `stardict_strcmp` (case-insensitive first). We find the first potential match and scan forward.

```rust
// Add inherent method:
impl StarDictDictionary {
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix_lower = prefix.to_lowercase();
        let mut results = Vec::new();

        // Binary search to find approximate start position
        let start = {
            let mut low = 0usize;
            let mut high = self.idx.entry_count();
            while low < high {
                let mid = low + (high - low) / 2;
                if self.idx.word_at(mid).to_lowercase() < prefix_lower {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            low
        };

        // Scan forward collecting matches
        for i in start..self.idx.entry_count() {
            let word = self.idx.word_at(i);
            if word.to_lowercase().starts_with(&prefix_lower) {
                results.push(word.to_string());
                if results.len() >= limit {
                    break;
                }
            } else if word.to_lowercase() > prefix_lower
                && !word.to_lowercase().starts_with(&prefix_lower)
            {
                break;
            }
        }

        results
    }
}

// Add trait impl:
impl Dictionary for StarDictDictionary {
    // ... existing methods ...

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.search_prefix(prefix, limit)
    }
}
```

### Modify: `src/mdict/mod.rs` — MDict prefix search

MDict keywords aren't sorted in a useful order by default. Build a sorted index during open for prefix search.

```rust
// In MdictDictionary struct, add:
    sorted_keys: Vec<(String, usize)>,  // (lowercased word, index into keywords)

// In open(), after building keyword_map:
    let mut sorted_keys: Vec<(String, usize)> = mdx.keywords
        .iter()
        .enumerate()
        .map(|(i, k)| (k.to_lowercase(), i))
        .collect();
    sorted_keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));

// Prefix search method:
impl MdictDictionary {
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix_lower = prefix.to_lowercase();
        let start = self.sorted_keys.partition_point(|(k, _)| k.as_str() < prefix_lower.as_str());

        let mut results = Vec::new();
        for (key, idx) in &self.sorted_keys[start..] {
            if key.starts_with(&prefix_lower) {
                results.push(self.mdx.keywords[*idx].clone());
                if results.len() >= limit {
                    break;
                }
            } else {
                break;
            }
        }
        results
    }
}

impl Dictionary for MdictDictionary {
    // ... existing methods ...

    fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        self.search_prefix(prefix, limit)
    }
}
```

---

## New Dependencies Summary

```toml
[dependencies]
anyhow = "1"
flate2 = "1"
memmap2 = "0.9"
encoding_rs = "0.8"
adler2 = "2"

[dependencies.xxhash-rust]
version = "0.8"
features = ["xxh64"]
```

## New Files Summary

```
src/mdict/encoding.rs   — unified byte-to-string decoding (GBK, Big5, UTF-8, UTF-16)
src/mdict/keygen.rs     — encryption key derivation (v2 ripemd128, v3 xxhash)
src/mdict/file.rs       — shared MdictFile struct for MDX/MDD parsing
```

## Modified Files Summary

```
Cargo.toml              — add encoding_rs, adler2, xxhash-rust
src/lib.rs              — add search_prefix to Dictionary trait
src/mdict/mod.rs        — refactor to use MdictFile, add MDD loading, encoding, prefix search
src/mdict/header.rs     — add uuid field
src/mdict/keys.rs       — encoding-aware keyword parsing, global_key param, checksum verification
src/mdict/records.rs    — version + global_key params
src/mdict/decompress.rs — version + global_key params, adler32 verification
src/stardict/mod.rs     — add search_prefix implementation + trait method
```

## Implementation Order

The features build on each other:

1. **GBK/Big5** — standalone, no deps on other features. Touches `keys.rs` and `mod.rs`.
2. **Adler32** — standalone, adds `version` param threading. Touches `decompress.rs`, `keys.rs`, `records.rs`, `mod.rs`.
3. **v3.0 encrypted** — builds on adler32 (uses version param). Adds `global_key` param threading. Touches `header.rs`, `decompress.rs`, `keys.rs`, `records.rs`, `mod.rs`.
4. **MDD** — builds on all above (MDD files can use any encoding/version/encryption). Refactors `mod.rs` into `file.rs` + `mod.rs`.
5. **Prefix search** — builds on MDD refactor (uses `mdx.keywords`). Touches `lib.rs`, `stardict/mod.rs`, `mdict/mod.rs`.

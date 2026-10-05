# MDict Format Support

Current status of MDict (.mdx/.mdd) format support and remaining work.

## Supported

### Core parsing (v2.0, v3.0)
- Header section: UTF-16LE XML parsing, all standard attributes (including UUID for v3)
- Keyword section: keyword index + keyword blocks, with decryption and decompression
- Record section: record index + record blocks, on-demand decompression
- Adler32 checksum verification (v2: decompressed data, v3: decrypted data)

### Encryption
- **Keyword index encryption** (Encrypted bit 2):
  - v2: nibble-swap XOR with RIPEMD-128 key derived from `ripemd128(adler32_checksum + 0x95360000)`
  - v3: nibble-swap XOR with global key derived from `xxh64(uuid[:mid]) ++ xxh64(uuid[mid:])`
- **Per-block fast_decrypt** (encryption method 1 in block info field): same nibble-swap XOR

### Compression
- None (comp_type 0)
- zlib (comp_type 2)

### Encodings
- UTF-8
- UTF-16 / UTF-16LE
- GBK / GB2312 / GB18030
- Big5
- Any encoding supported by the `encoding_rs` crate

### MDD Resource Files
- Companion `.mdd` files loaded alongside `.mdx` (same_name.mdd, same_name.1.mdd, etc.)
- `lookup_resource(path: &str) -> Option<Vec<u8>>` for CSS, images, fonts, JS
- Auto-detects UTF-16LE encoding for MDD files with empty Encoding header

### Lookup
- Exact match via HashMap (case-insensitive by default, respects `KeyCaseSensitive` header attribute)
- Prefix search via sorted index + `partition_point` (case-insensitive)
- `search_prefix()` available on both `MdictDictionary` and the `Dictionary` trait

---

## TODO

### Priority 1 — Worth doing

#### Fuzzy / similarity search
Beyond prefix matching, support approximate matching for typo tolerance.

- **Approach**: Levenshtein distance or similar over the sorted word list. Could use a BK-tree for efficient fuzzy queries. Keep this behind a feature flag to avoid bloating the core.

### Priority 2 — Edge cases

#### MDict v1.2 format
Older dictionaries use v1.2 which has different field sizes throughout:

- Keyword section header: 4-byte fields instead of 8-byte
- Key index: different structure (1-byte word length prefix instead of 2-byte)
- Record offsets: 4 bytes instead of 8
- No compression in key index (raw data, not wrapped in comp_type+checksum blocks)
- **Where**: Version-conditional parsing in `keys.rs` and `records.rs`

#### Keyword header encryption (Encrypted bit 1)
Salsa20/8 encryption of the 40-byte keyword section header. Rare in practice.

- Requires decryption key from either a `.key` file (same name as `.mdx`) or `RegCode` header attribute
- Key derivation: `salsa20_8(ripemd128(user_id))` decrypts the reg_code to get the actual key
- **Dependency**: Would need a Salsa20 implementation (pure Rust or crate)

#### Per-block Salsa20 encryption (method 2)
Some blocks can use Salsa20 instead of nibble-swap XOR. Currently errors.

- Same Salsa20/8 implementation needed as keyword header encryption

#### LZO compression (comp_type 1)
Some older dictionaries use LZO compression. Currently errors on these blocks.

- **Dependency**: `lzo1x` or similar crate

#### StyleSheet / Compact mode
MDict-specific text substitution scheme where numbered placeholders in records are replaced with strings defined in the `StyleSheet` header attribute. Rarely used.

---

## Architecture notes

### File layout
```
src/mdict/
  mod.rs          — MdictDictionary struct, Dictionary trait impl, file discovery
  file.rs         — MdictFile: shared parsing struct for MDX and MDD files
  header.rs       — Header section parsing (UTF-16LE XML attributes)
  keys.rs         — Keyword section: header, index (with decryption), blocks
  records.rs      — Record section: index, on-demand block decompression
  decompress.rs   �� Block decompression (none/zlib) + per-block decryption + adler32
  encoding.rs     — Multi-encoding byte-to-string decoding (UTF-8, UTF-16, GBK, Big5, etc.)
  keygen.rs       — v3 key derivation (xxhash64-based)
  ripemd128.rs    — RIPEMD-128 hash (for v2 key derivation)
  decrypt.rs      — Nibble-swap XOR decryption (fast_decrypt)
```

### Memory model
The entire `.mdx`/`.mdd` file is read into `Vec<u8>`. Keywords and record offsets are parsed eagerly. Record blocks are decompressed on demand during lookup. The HashMap for lookups duplicates keyword strings (lowercased for case-insensitive mode). A sorted `Vec<(String, usize)>` is maintained for prefix search.

For large dictionaries (200k+ entries), the HashMap + keyword Vec + sorted keys can use significant memory. A future optimization could unify these into a single sorted Vec with binary search and case-folded comparison.

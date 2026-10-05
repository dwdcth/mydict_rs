# Medium Priority Fixes — Implementation Plan

Items 3, 4, 6, 7 from known-issues.md. Item 5 (`#[non_exhaustive]`) was already done in the high-priority pass.

---

## Issue 3: Remove unnecessary `unsafe` in Idx and Syn

Replace `str::from_utf8_unchecked` with `str::from_utf8().unwrap()`. UTF-8 is validated during `open()`, so `unwrap()` is equally safe but without the `unsafe` footgun.

### `src/stardict/idx.rs` — `word_at()` (line 58-65)

```rust
// Before:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    // Safety: validated UTF-8 during open()
    unsafe { str::from_utf8_unchecked(&self.data[start..start + null_pos]) }
}

// After:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    str::from_utf8(&self.data[start..start + null_pos]).unwrap()
}
```

### `src/stardict/idx.rs` — `entry()` (line 69-77)

```rust
// Before:
let word = unsafe {
    str::from_utf8_unchecked(&self.data[start..start + null_pos])
}.to_string();

// After:
let word = str::from_utf8(&self.data[start..start + null_pos])
    .unwrap()
    .to_string();
```

### `src/stardict/syn.rs` — `word_at()` (line 55-61)

```rust
// Before:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    unsafe { str::from_utf8_unchecked(&self.data[start..start + null_pos]) }
}

// After:
pub(crate) fn word_at(&self, i: usize) -> &str {
    let start = self.offsets[i] as usize;
    let null_pos = self.data[start..]
        .iter()
        .position(|&b| b == 0)
        .unwrap();
    str::from_utf8(&self.data[start..start + null_pos]).unwrap()
}
```

### `src/stardict/syn.rs` — `entry()` (line 64-72)

```rust
// Before:
let word = unsafe {
    str::from_utf8_unchecked(&self.data[start..start + null_pos])
}.to_string();

// After:
let word = str::from_utf8(&self.data[start..start + null_pos])
    .unwrap()
    .to_string();
```

---

## Issue 4: Fix all clippy warnings

### 4a. `map_or(false, ...)` → `is_some_and()` (4 instances)

**`src/stardict/dict.rs:30-31`**:
```rust
// Before:
let is_compressed = file.extension()
    .map_or(false, |ext| ext == "gz" || ext == "dz");
// After:
let is_compressed = file.extension()
    .is_some_and(|ext| ext == "gz" || ext == "dz");
```

**`src/stardict/io.rs:10`**:
```rust
// Before:
if path.extension().map_or(false, |ext| ext == "gz" || ext == "dz") {
// After:
if path.extension().is_some_and(|ext| ext == "gz" || ext == "dz") {
```

**`src/stardict/mod.rs:151`** (in `find_file_with_ext`):
```rust
// Before:
if path.extension().map_or(false, |e| e == ext) {
// After:
if path.extension().is_some_and(|e| e == ext) {
```

**`src/mdict/mod.rs:152`** (in `find_mdx`):
```rust
// Before:
if path.extension().map_or(false, |e| e.eq_ignore_ascii_case("mdx")) {
// After:
if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mdx")) {
```

### 4b. Redundant closures (2 instances)

**`src/stardict/idx.rs:108`**:
```rust
// Before:
self.binary_search(word, |w, target| stardict_strcmp(w, target))
// After:
self.binary_search(word, stardict_strcmp)
```

**`src/stardict/syn.rs:82`**:
```rust
// Before:
self.binary_search(word, |w, target| stardict_strcmp(w, target))
// After:
self.binary_search(word, stardict_strcmp)
```

### 4c. Manual range check → `contains()`

**`src/stardict/strcmp.rs:36`**:
```rust
// Before:
fn ascii_lower(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' {
        b + (b'a' - b'A')
    } else {
        b
    }
}
// After:
fn ascii_lower(b: u8) -> u8 {
    if (b'A'..=b'Z').contains(&b) {
        b + (b'a' - b'A')
    } else {
        b
    }
}
```

### 4d. Manual `div_ceil`

**`src/mdict/keygen.rs:9`**:
```rust
// Before:
let mid = (uuid.len() + 1) / 2;
// After:
let mid = uuid.len().div_ceil(2);
```

### 4e. Manual bit rotation (2 instances)

**`src/mdict/ripemd128.rs:80`**:
```rust
// Before:
fn rol(s: u32, x: u32) -> u32 {
    (x << s) | (x >> (32 - s))
}
// After:
fn rol(s: u32, x: u32) -> u32 {
    x.rotate_left(s)
}
```

**`src/mdict/decrypt.rs:12`**:
```rust
// Before:
let swapped = (byte >> 4) | (byte << 4);
// After:
let swapped = byte.rotate_left(4);
```

### 4f. Complex return type

**`src/mdict/records.rs`** — add a type alias at the top:
```rust
// Add before parse_record_index:
/// (compressed_offset, compressed_size, decompressed_size) per block.
pub type RecordBlockInfo = (u64, u64, u64);

// Then change the signature:
// Before:
pub fn parse_record_index(
    data: &[u8],
    start: usize,
    _header: &MdictHeader,
) -> crate::Result<(Vec<(u64, u64, u64)>, u64)> {
// After:
pub fn parse_record_index(
    data: &[u8],
    start: usize,
    _header: &MdictHeader,
) -> crate::Result<(Vec<RecordBlockInfo>, u64)> {
```

Update the one consumer in `src/mdict/file.rs:17`:
```rust
// Before:
pub record_blocks: Vec<(u64, u64, u64)>,
// After:
pub record_blocks: Vec<records::RecordBlockInfo>,
```

### 4g. Empty line after doc comment

**`src/mdict/ripemd128.rs:1-5`**:
```rust
// Before:
/// RIPEMD-128 hash function.
/// Ported from the Python reference implementation by zhansliu/writemdict.
/// Reference: http://homes.esat.kuleuven.be/~bosselae/ripemd/rmd128.txt

pub fn ripemd128(message: &[u8]) -> [u8; 16] {

// After (remove the blank line):
/// RIPEMD-128 hash function.
/// Ported from the Python reference implementation by zhansliu/writemdict.
/// Reference: http://homes.esat.kuleuven.be/~bosselae/ripemd/rmd128.txt
pub fn ripemd128(message: &[u8]) -> [u8; 16] {
```

---

## Issue 6: Remove dead parameter `Syn::open(_syn_word_count)`

### `src/stardict/syn.rs:22`

```rust
// Before:
pub fn open(file: path::PathBuf, _syn_word_count: u32) -> crate::Result<Syn> {
// After:
pub fn open(file: path::PathBuf) -> crate::Result<Syn> {
```

### `src/stardict/mod.rs:57` (call site)

```rust
// Before:
Some(syn::Syn::open(syn_path, ifo.syn_word_count as u32)?)
// After:
Some(syn::Syn::open(syn_path)?)
```

### `src/stardict/syn.rs` tests — remove the second argument from all `Syn::open()` calls in the test module:
- Line 131: `Syn::open(fixture("testdict.syn"), 2)` → `Syn::open(fixture("testdict.syn"))`
- Line 137: same
- Line 145: same
- Line 153: same
- Line 159 (raw data test, no `open` call)
- Line 167 (raw data test, no `open` call)
- Line 175: `Syn::open(fixture("testdict.syn"), 2)` → `Syn::open(fixture("testdict.syn"))`
- Line 194: `Syn::open(path, 3)` → `Syn::open(path)`
- Line 212: `Syn::open(fixture("testdict.syn"), 2)` → `Syn::open(fixture("testdict.syn"))`

---

## Issue 7: Add `Debug` to public and internal types

### `src/stardict/mod.rs`

```rust
// Before:
pub struct StarDictDictionary {
// After:
#[derive(Debug)]
pub struct StarDictDictionary {
```

Note: `Dict` contains `DictData` which contains `Mmap`. `Mmap` implements `Debug`, so this works. `Ifo` already derives `Debug`.

### `src/stardict/idx.rs`

```rust
// Before:
pub struct Idx {
// After:
#[derive(Debug)]
pub struct Idx {
```

### `src/stardict/dict.rs`

`Dict` contains `DictData` (an enum with `Mmap` and `Vec<u8>`). Need `Debug` on `DictData` too:

```rust
// Before:
enum DictData {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

pub struct Dict {
// After:
#[derive(Debug)]
enum DictData {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

#[derive(Debug)]
pub struct Dict {
```

### `src/stardict/syn.rs`

```rust
// Before:
pub struct Syn {
// After:
#[derive(Debug)]
pub struct Syn {
```

### `src/mdict/mod.rs`

`MdictDictionary` contains `MdictFile` which has a `Mutex`. `Mutex` implements `Debug`, so this works.

```rust
// Before:
pub struct MdictDictionary {
// After:
#[derive(Debug)]
pub struct MdictDictionary {
```

### `src/mdict/file.rs`

`MdictFile` contains `Mmap`, `Mutex<Option<(usize, Arc<Vec<u8>>)>>` — all implement `Debug`.

```rust
// Before:
pub struct MdictFile {
// After:
#[derive(Debug)]
pub struct MdictFile {
```

### `src/mdict/header.rs`

```rust
// Before:
pub struct MdictHeader {
// After:
#[derive(Debug)]
pub struct MdictHeader {
```

---

## Execution Order

1. **Remove `unsafe`** (Issue 3) — safe, mechanical
2. **Fix clippy warnings** (Issue 4) — mechanical, no logic changes
3. **Remove dead parameter** (Issue 6) — small signature change
4. **Add `Debug`** (Issue 7) — additive, no logic changes

## Files Modified

| File | Issues |
|------|--------|
| `src/stardict/idx.rs` | 3 (unsafe), 4b (closure) |
| `src/stardict/syn.rs` | 3 (unsafe), 4b (closure), 6 (dead param) |
| `src/stardict/dict.rs` | 4a (map_or), 7 (Debug) |
| `src/stardict/io.rs` | 4a (map_or) |
| `src/stardict/strcmp.rs` | 4c (range check) |
| `src/stardict/mod.rs` | 4a (map_or), 6 (call site), 7 (Debug) |
| `src/mdict/mod.rs` | 4a (map_or), 7 (Debug) |
| `src/mdict/keygen.rs` | 4d (div_ceil) |
| `src/mdict/ripemd128.rs` | 4e (rotate), 4g (doc comment) |
| `src/mdict/decrypt.rs` | 4e (rotate) |
| `src/mdict/records.rs` | 4f (type alias) |
| `src/mdict/file.rs` | 4f (use type alias), 7 (Debug) |
| `src/mdict/header.rs` | 7 (Debug) |

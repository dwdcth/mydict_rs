# MDict: mmap instead of reading entire file into memory

## Problem

`MdictFile::open` (`src/mdict/file.rs:25`) does `std::fs::read(path)?`, loading the entire `.mdx`/`.mdd` file into a `Vec<u8>`. Large dictionaries (Oxford OED .mdx is 100MB+, plus companion .mdd files) eat significant memory.

StarDict already uses `mmap` for its `.dict` data (`src/stardict/dict.rs`). MDict should do the same.

## Why this is safe

- `Mmap` implements `Deref<Target = [u8]>`, so all existing `&self.data[...]` slice operations work unchanged
- `Mmap` implements `Send + Sync` (needed for `Box<dyn Dictionary + Send + Sync>`)
- `memmap2` is already a dependency
- All parsing functions (`parse_header`, `parse_keywords`, `parse_record_index`) take `&[u8]` — no signature changes needed
- Dictionary files are read-only; we never modify them

## Changes

### `src/mdict/file.rs`

Add imports:

```diff
 use std::collections::HashMap;
+use std::fs::File;
 use std::sync::{Arc, Mutex};

+use memmap2::Mmap;
+
 use super::{header, keys, records, keygen, decompress};
 use super::header::MdictHeader;
```

Change struct field:

```diff
 pub struct MdictFile {
     ...
-    pub data: Vec<u8>,
+    pub data: Mmap,
     ...
 }
```

Change file loading in `open()`:

```diff
-        let data = std::fs::read(path)?;
+        let file = File::open(path)?;
+        // SAFETY: dictionary files are read-only; we do not modify them.
+        let data = unsafe { Mmap::map(&file)? };
```

That's it. Three lines changed, one field type changed.

## Verification

```
cargo test
cargo build --release
```

All existing tests pass — the parsing code sees the same `&[u8]` slices it always did.

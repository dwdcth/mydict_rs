# Low-hanging fruit — exact changes

## 1. Fix `.gitignore` stale names

`/.gitignore` lines 26-29:

```diff
-/expo/ios/stardict_mobile.swift
-/expo/ios/stardict_mobileFFI.h
-/expo/ios/stardict_mobileFFI.modulemap
-/expo/ios/libstardict_mobile.a
+/expo/ios/opendict_mobile.swift
+/expo/ios/opendict_mobileFFI.h
+/expo/ios/opendict_mobileFFI.modulemap
+/expo/ios/libopendict_mobile.a
```

## 2. Delete dead `records::read_record`

`src/mdict/records.rs` — remove the unused import and the entire function (lines 1-3, 48-103):

Remove import (line 3):
```diff
 use anyhow::anyhow;
 use crate::mdict::header::MdictHeader;
-use crate::mdict::decompress;
```

Delete the entire `read_record` function (lines 48-103):
```diff
-/// Read a single record by keyword index.
-pub fn read_record(
-    data: &[u8],
-    record_blocks_start: u64,
-    record_blocks: &[(u64, u64, u64)],
-    record_offsets: &[u64],
-    keyword_index: usize,
-    version: f32,
-    global_key: Option<&[u8; 16]>,
-) -> anyhow::Result<Vec<u8>> {
-    ... (entire function body, through line 103)
-}
```

## 3. Fix `unwrap()` panics

### `src/stardict/dict.rs` — two null-terminator lookups

Line 119 (inside `Some(sts)` arm, non-last lowercase field):
```diff
-                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
+                        let null_pos = match raw.iter().position(|&b| b == 0) {
+                            Some(p) => p,
+                            None => break, // malformed: no null terminator
+                        };
```

Line 142 (inside `None` arm, no sametypesequence):
```diff
-                        let null_pos = raw.iter().position(|&b| b == 0).unwrap();
+                        let null_pos = match raw.iter().position(|&b| b == 0) {
+                            Some(p) => p,
+                            None => break, // malformed: no null terminator
+                        };
```

### `src/stardict/mod.rs` — filename unwraps

Line 25 (inside `open_dir`):
```diff
-        let name = ifo_path.file_stem().unwrap().to_str().unwrap().to_string();
+        let name = ifo_path
+            .file_stem()
+            .and_then(|s| s.to_str())
+            .ok_or_else(|| anyhow!("invalid .ifo filename: {}", ifo_path.display()))?
+            .to_string();
```

## 4. Unit tests for `encoding.rs`

Add `#[cfg(test)]` block at the bottom of `src/mdict/encoding.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── decode_str ──────────────────────────────────────────────

    #[test]
    fn utf8_passthrough() {
        assert_eq!(decode_str(b"hello", "UTF-8"), "hello");
    }

    #[test]
    fn utf8_label_variants() {
        assert_eq!(decode_str(b"abc", "utf-8"), "abc");
        assert_eq!(decode_str(b"abc", "UTF8"), "abc");
        assert_eq!(decode_str(b"abc", "utf8"), "abc");
    }

    #[test]
    fn utf16le_decode() {
        // "hi" in UTF-16LE: h=0x0068, i=0x0069
        let bytes = [0x68, 0x00, 0x69, 0x00];
        assert_eq!(decode_str(&bytes, "UTF-16LE"), "hi");
    }

    #[test]
    fn utf16le_label_variants() {
        let bytes = [0x41, 0x00]; // "A"
        assert_eq!(decode_str(&bytes, "UTF-16"), "A");
        assert_eq!(decode_str(&bytes, "utf-16le"), "A");
        assert_eq!(decode_str(&bytes, "UTF16"), "A");
    }

    #[test]
    fn utf16le_cjk() {
        // U+4F60 (你) in UTF-16LE: 0x60 0x4F
        let bytes = [0x60, 0x4F];
        assert_eq!(decode_str(&bytes, "UTF-16LE"), "你");
    }

    #[test]
    fn gbk_decode() {
        // "你" in GBK: 0xC4 0xE3
        let bytes = [0xC4, 0xE3];
        assert_eq!(decode_str(&bytes, "GBK"), "你");
    }

    #[test]
    fn gb18030_decode() {
        // "你" in GB18030: same as GBK for BMP characters
        let bytes = [0xC4, 0xE3];
        assert_eq!(decode_str(&bytes, "GB18030"), "你");
    }

    #[test]
    fn big5_decode() {
        // "你" in Big5: 0xA9 0x41... actually let's use a simpler char
        // "A" is just 0x41 in Big5 (ASCII range)
        assert_eq!(decode_str(b"ABC", "Big5"), "ABC");
    }

    #[test]
    fn unknown_encoding_falls_back_to_utf8() {
        assert_eq!(decode_str(b"test", "TOTALLY-FAKE"), "test");
    }

    #[test]
    fn empty_input() {
        assert_eq!(decode_str(b"", "UTF-8"), "");
        assert_eq!(decode_str(b"", "UTF-16LE"), "");
        assert_eq!(decode_str(b"", "GBK"), "");
    }

    // ── null_width ──────────────────────────────────────────────

    #[test]
    fn null_width_utf16_variants() {
        assert_eq!(null_width("UTF-16"), 2);
        assert_eq!(null_width("UTF-16LE"), 2);
        assert_eq!(null_width("utf-16"), 2);
        assert_eq!(null_width("utf-16le"), 2);
        assert_eq!(null_width("UTF16"), 2);
    }

    #[test]
    fn null_width_single_byte_encodings() {
        assert_eq!(null_width("UTF-8"), 1);
        assert_eq!(null_width("GBK"), 1);
        assert_eq!(null_width("GB18030"), 1);
        assert_eq!(null_width("Big5"), 1);
        assert_eq!(null_width(""), 1);
    }
}
```

## 5. Unit tests for `keygen.rs`

Add `#[cfg(test)]` block at the bottom of `src/mdict/keygen.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_returns_none() {
        assert!(derive_key(2.0, Some(b"anything")).is_none());
        assert!(derive_key(2.0, None).is_none());
    }

    #[test]
    fn v3_no_uuid_returns_none() {
        assert!(derive_key(3.0, None).is_none());
    }

    #[test]
    fn v3_with_uuid_returns_16_byte_key() {
        let key = derive_key(3.0, Some(b"test-uuid")).unwrap();
        assert_eq!(key.len(), 16);
    }

    #[test]
    fn v3_deterministic() {
        let k1 = derive_key(3.0, Some(b"test-uuid")).unwrap();
        let k2 = derive_key(3.0, Some(b"test-uuid")).unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn v3_different_uuids_differ() {
        let k1 = derive_key(3.0, Some(b"uuid-aaa")).unwrap();
        let k2 = derive_key(3.0, Some(b"uuid-bbb")).unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn v3_key_matches_xxhash64_split() {
        // UUID "abcdef" (6 bytes) → mid = (6+1)/2 = 3
        // left = b"abc", right = b"def"
        let uuid = b"abcdef";
        let key = derive_key(3.0, Some(uuid)).unwrap();

        let h1 = xxhash_rust::xxh64::xxh64(b"abc", 0);
        let h2 = xxhash_rust::xxh64::xxh64(b"def", 0);
        assert_eq!(&key[..8], &h1.to_le_bytes());
        assert_eq!(&key[8..], &h2.to_le_bytes());
    }

    #[test]
    fn v3_single_byte_uuid() {
        // UUID "x" (1 byte) → mid = 1, left = b"x", right = b""
        let key = derive_key(3.0, Some(b"x")).unwrap();
        let h1 = xxhash_rust::xxh64::xxh64(b"x", 0);
        let h2 = xxhash_rust::xxh64::xxh64(b"", 0);
        assert_eq!(&key[..8], &h1.to_le_bytes());
        assert_eq!(&key[8..], &h2.to_le_bytes());
    }
}
```

## 6. Unit tests for `header.rs`

The functions `parse_xml_attrs` and `decode_utf16le` are private. Either make them `pub(crate)` or add tests inside `header.rs`. Since they're implementation details, testing inside the module is cleaner.

First, change visibility on line 88 and line 77:

```diff
-fn decode_utf16le(data: &[u8]) -> anyhow::Result<String> {
+pub(crate) fn decode_utf16le(data: &[u8]) -> anyhow::Result<String> {
```

```diff
-fn parse_xml_attrs(xml: &str) -> Vec<(String, String)> {
+pub(crate) fn parse_xml_attrs(xml: &str) -> Vec<(String, String)> {
```

Then add `#[cfg(test)]` block at the bottom of `src/mdict/header.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // ── decode_utf16le ──────────────────────────────────────────

    #[test]
    fn decodes_ascii() {
        // "Hi" → H=0x0048 i=0x0069
        let bytes = [0x48, 0x00, 0x69, 0x00];
        assert_eq!(decode_utf16le(&bytes).unwrap(), "Hi");
    }

    #[test]
    fn decodes_cjk() {
        // U+4F60 (你) → 0x60 0x4F in LE
        let bytes = [0x60, 0x4F];
        assert_eq!(decode_utf16le(&bytes).unwrap(), "你");
    }

    #[test]
    fn empty_input() {
        assert_eq!(decode_utf16le(&[]).unwrap(), "");
    }

    #[test]
    fn odd_byte_count_is_error() {
        assert!(decode_utf16le(&[0x00]).is_err());
    }

    // ── parse_xml_attrs ─────────────────────────────────────────

    #[test]
    fn single_attr_double_quotes() {
        let attrs = parse_xml_attrs(r#"<Dict Title="Test">"#);
        assert_eq!(attrs, vec![("Title".to_string(), "Test".to_string())]);
    }

    #[test]
    fn single_attr_single_quotes() {
        let attrs = parse_xml_attrs("<Dict Title='Test'>");
        assert_eq!(attrs, vec![("Title".to_string(), "Test".to_string())]);
    }

    #[test]
    fn multiple_attrs() {
        let attrs = parse_xml_attrs(
            r#"<Dict GeneratedByEngineVersion="2.0" Encoding="UTF-8" Format="Html">"#,
        );
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0], ("GeneratedByEngineVersion".to_string(), "2.0".to_string()));
        assert_eq!(attrs[1], ("Encoding".to_string(), "UTF-8".to_string()));
        assert_eq!(attrs[2], ("Format".to_string(), "Html".to_string()));
    }

    #[test]
    fn empty_value() {
        let attrs = parse_xml_attrs(r#"<Dict Title="">"#);
        assert_eq!(attrs, vec![("Title".to_string(), String::new())]);
    }

    #[test]
    fn no_attrs() {
        let attrs = parse_xml_attrs("<Dict>");
        assert!(attrs.is_empty());
    }

    #[test]
    fn empty_string() {
        let attrs = parse_xml_attrs("");
        assert!(attrs.is_empty());
    }

    #[test]
    fn value_with_spaces() {
        let attrs = parse_xml_attrs(r#"<Dict Title="My Cool Dict">"#);
        assert_eq!(attrs, vec![("Title".to_string(), "My Cool Dict".to_string())]);
    }

    #[test]
    fn newlines_between_attrs() {
        let attrs = parse_xml_attrs(
            "<Dict\nTitle=\"Test\"\nEncoding=\"UTF-8\">",
        );
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].0, "Title");
        assert_eq!(attrs[1].0, "Encoding");
    }
}
```

## Summary

| Change | Files touched | Lines added | Lines removed |
|--------|--------------|-------------|---------------|
| .gitignore fix | 1 | 4 | 4 |
| Delete dead read_record | 1 | 0 | ~57 |
| Fix unwrap panics | 2 | 10 | 3 |
| encoding.rs tests | 1 | ~75 | 0 |
| keygen.rs tests | 1 | ~55 | 0 |
| header.rs tests | 1 | ~75 | 0 |
| **Total** | **5 files** | **~219** | **~64** |

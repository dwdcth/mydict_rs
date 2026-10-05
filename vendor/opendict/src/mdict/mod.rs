// MYDICT-PATCH 3: header 模块改为 pub —— 适配层需要读 engine 版本/StyleSheet/Compact。
pub mod header;
// MYDICT-PATCH 3: 顶层 re-export，使用方可写 `opendict::mdict::MdictHeader`。
pub use header::MdictHeader;
pub(crate) mod keys;
pub(crate) mod records;
pub(crate) mod decompress;
pub(crate) mod ripemd128;
pub(crate) mod decrypt;
pub(crate) mod encoding;
pub(crate) mod keygen;
pub(crate) mod file;

use std::path;

use crate::error::Error;
use crate::types::{DictEntry, DictInfo};
use crate::Dictionary;

#[derive(Debug)]
pub struct MdictDictionary {
    info_data: DictInfo,
    mdx: file::MdictFile,
    mdd: Vec<file::MdictFile>,
    format_type: char,
    case_sensitive: bool,
    encoding: String,
    // Sorted (lowercased_key, original_index) for prefix search
    sorted_keys: Vec<(String, usize)>,
}

impl MdictDictionary {
    pub fn open(dir: &path::Path) -> crate::Result<Self> {
        let mdx_path = find_mdx(dir)?;
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
            word_count: mdx.keywords.len(),
        };

        // Build sorted index for prefix search
        let mut sorted_keys: Vec<(String, usize)> = mdx
            .keywords
            .iter()
            .enumerate()
            .map(|(i, k)| (k.to_lowercase(), i))
            .collect();
        sorted_keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Load .mdd resource files alongside .mdx
        let mdd = load_mdd_files(&mdx_path);

        Ok(MdictDictionary {
            info_data,
            mdx,
            mdd,
            format_type,
            case_sensitive,
            encoding,
            sorted_keys,
        })
    }

    /// Look up a resource (CSS, image, font, etc.) from .mdd files.
    /// Path should match the MDD keyword format, e.g. `\style.css` or `/style.css`.
    pub fn lookup_resource(&self, path: &str) -> Option<Vec<u8>> {
        // Normalise path separators (MDD uses backslash internally)
        let normalised = path.replace('/', "\\");
        let lookup_key = normalised.to_lowercase();
        for mdd in &self.mdd {
            if let Ok(Some(data)) = mdd.lookup_raw(&lookup_key) {
                return Some(data);
            }
        }
        None
    }

    /// MYDICT-PATCH 1: enumerate every MDD resource key (original case, MDD
    /// backslash separators). The mydict importer unpacks all resources at
    /// import time, so it needs the full key list, not point lookups.
    pub fn mdd_resource_keys(&self) -> Vec<String> {
        self.mdd
            .iter()
            .flat_map(|mdd| mdd.keywords.iter().cloned())
            .collect()
    }

    /// MYDICT-PATCH 1: positional entry access — keyword by index plus its
    /// decoded record text. Unlike `Dictionary::lookup`, duplicate headwords
    /// stay distinct and no case normalisation is applied.
    pub fn entry_at(&self, i: usize) -> crate::Result<Option<(String, String)>> {
        if i >= self.mdx.keywords.len() {
            return Ok(None);
        }
        let word = self.mdx.keywords[i].clone();
        let raw = self.mdx.record_at(i)?;
        let text = encoding::decode_str(&raw, &self.encoding);
        Ok(Some((word, text)))
    }

    /// MYDICT-PATCH 6: headword-only positional access (mydict lite import
    /// mode) — reads the key index without touching any record block.
    pub fn word_at(&self, i: usize) -> Option<&str> {
        self.mdx.keywords.get(i).map(String::as_str)
    }

    /// MYDICT-PATCH 1: keyword count (equals entry count).
    pub fn entry_count(&self) -> usize {
        self.mdx.keywords.len()
    }

    /// MYDICT-PATCH 3: expose the parsed MDX header (engine version,
    /// StyleSheet, Compact, encoding, ...) to the mydict importer.
    pub fn header(&self) -> &header::MdictHeader {
        &self.mdx.header
    }

    /// Prefix search: find words starting with `prefix`, up to `limit` results.
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix_lower = prefix.to_lowercase();
        let start = self
            .sorted_keys
            .partition_point(|(k, _)| k.as_str() < prefix_lower.as_str());

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

        // Decode record bytes from source encoding to UTF-8
        let decoded = encoding::decode_str(&record_data, &self.encoding);

        Ok(Some(vec![DictEntry {
            type_id: self.format_type,
            data: decoded.into_bytes(),
        }]))
    }

    fn lookup_synonym(&self, _word: &str) -> crate::Result<Option<Vec<DictEntry>>> {
        Ok(None)
    }

    fn word_list(&self) -> Vec<&str> {
        self.mdx.keywords.iter().map(String::as_str).collect()
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
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mdx")) {
            return Ok(path);
        }
    }
    Err(Error::InvalidFormat(format!(
        "no .mdx file found in {}", dir.display()
    )))
}

/// Find and load .mdd files alongside the .mdx file.
/// Looks for: same_name.mdd, same_name.1.mdd, same_name.2.mdd, ...
fn load_mdd_files(mdx_path: &std::path::Path) -> Vec<file::MdictFile> {
    let stem = match mdx_path.file_stem() {
        Some(s) => s.to_string_lossy().to_lowercase(),
        None => return Vec::new(),
    };
    let dir = match mdx_path.parent() {
        Some(d) => d,
        None => return Vec::new(),
    };

    let mut mdds = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        let fname = path.file_name().unwrap().to_string_lossy().to_lowercase();
        if fname.ends_with(".mdd") && fname.starts_with(&stem) {
            match file::MdictFile::open(&path, Some(false)) {
                Ok(mdd) => mdds.push(mdd),
                Err(e) => log::warn!("failed to load MDD {}: {}", path.display(), e),
            }
        }
    }

    mdds
}

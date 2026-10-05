//! StarDict 解析器 —— 移植自 `app/parsers/stardict.py`。
//!
//! parse() 走 opendict-rs（mmap 零拷贝 + fork 补丁的位置访问/同义词枚举，.dz 解压
//! 与 Python 一样整份进内存，cache_to_disk=false 不写用户词典目录）；
//! sample/sample_headwords 直接读原始文件 —— 保持 Python 版的内存上限语义
//! （idx 前 1MB / dict 前 8MB / gz idx 全解压 32MB 封顶）。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::entry::{
    is_informative, is_informative_headword, spread_downsample, ParsedEntry, SAMPLE_SCAN_FACTOR,
};
use crate::{ParseOpts, ParserError, Result};

/// 采样最多读 .idx 的前 1MB
const SAMPLE_IDX_BYTES: usize = 1024 * 1024;
/// 采样释义时最多读 .dict 的前 8MB，防止异常偏移把 GB 级文件整份读进内存
const SAMPLE_DICT_BYTES: usize = 8 * 1024 * 1024;
/// .idx.gz 不能随机 seek，整份解压按此上限封顶
const SAMPLE_IDX_FULL_BYTES: usize = 32 * 1024 * 1024;
/// 从 .idx 中段取样时一次 seek 后读取的窗口大小（一条记录十几字节，4KB 足够）
const IDX_SEEK_CHUNK: usize = 4096;

pub struct StarDictParser;

fn parse_ifo(ifo_path: &Path) -> Result<HashMap<String, String>> {
    let mut meta = HashMap::new();
    let content = std::fs::read_to_string(ifo_path)?;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("StarDict") {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            meta.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(meta)
}

/// 按后缀归类（双后缀 dict.dz / idx.gz 先于单后缀判断）
fn index_by_suffix(file_paths: &[PathBuf]) -> HashMap<&'static str, &PathBuf> {
    let mut by_suffix = HashMap::new();
    for path in file_paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if name.ends_with(".dict.dz") {
            by_suffix.insert("dict", path);
        } else if name.ends_with(".idx.gz") {
            by_suffix.insert("idx_gz", path);
        } else {
            let suffix = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let key = match suffix.as_str() {
                "ifo" => Some("ifo"),
                "idx" => Some("idx"),
                "dict" => Some("dict"),
                "syn" => Some("syn"),
                _ => None,
            };
            if let Some(key) = key {
                by_suffix.entry(key).or_insert(path);
            }
        }
    }
    by_suffix
}

fn is_gzip(path: &Path, suffix: &str) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    suffix == "gz" || name.ends_with(".idx.gz") || name.ends_with(".dict.dz") || suffix == "dz"
}

/// 读取 .idx（或 .idx.gz）内容；size 限读前 N 字节
fn read_idx_bytes(idx_path: &Path, size: Option<usize>) -> Result<Vec<u8>> {
    read_maybe_gzip(idx_path, is_gzip(idx_path, "gz"), size)
}

/// 读取 .dict（或 .dict.dz）内容
fn read_dict_content(dict_path: &Path, size: Option<usize>) -> Result<Vec<u8>> {
    read_maybe_gzip(dict_path, is_gzip(dict_path, "dz"), size)
}

fn read_maybe_gzip(path: &Path, gzip: bool, size: Option<usize>) -> Result<Vec<u8>> {
    use std::io::Read as _;
    if gzip {
        let file = std::fs::File::open(path)?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        match size {
            Some(n) => {
                let mut buf = vec![0u8; n];
                let mut read_total = 0usize;
                loop {
                    let n = decoder.read(&mut buf[read_total..])?;
                    if n == 0 {
                        break;
                    }
                    read_total += n;
                    if read_total == buf.len() {
                        break;
                    }
                }
                buf.truncate(read_total);
                Ok(buf)
            }
            None => {
                let mut buf = Vec::new();
                decoder.read_to_end(&mut buf)?;
                Ok(buf)
            }
        }
    } else {
        let mut file = std::fs::File::open(path)?;
        match size {
            Some(n) => {
                let mut buf = vec![0u8; n];
                let mut read_total = 0usize;
                loop {
                    let n = file.read(&mut buf[read_total..])?;
                    if n == 0 {
                        break;
                    }
                    read_total += n;
                    if read_total == buf.len() {
                        break;
                    }
                }
                buf.truncate(read_total);
                Ok(buf)
            }
            None => {
                let mut buf = Vec::new();
                file.read_to_end(&mut buf)?;
                Ok(buf)
            }
        }
    }
}

/// (word, offset, length) 记录流；返回 None 表示剩余字节是一条被截断的半记录
fn iter_idx_entries(
    idx_bytes: &[u8],
    offset_bits: u32,
) -> impl Iterator<Item = Option<(String, u64, u32)>> + '_ {
    let offset_size = if offset_bits == 64 { 8usize } else { 4 };
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos >= idx_bytes.len() {
            return None;
        }
        let Some(end) = idx_bytes[pos..].iter().position(|&b| b == 0) else {
            pos = idx_bytes.len();
            return None;
        };
        let end = pos + end;
        let word = String::from_utf8_lossy(&idx_bytes[pos..end]).into_owned();
        pos = end + 1;
        if pos + offset_size + 4 > idx_bytes.len() {
            // 半条记录
            pos = idx_bytes.len();
            return Some(None);
        }
        let offset = if offset_size == 8 {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&idx_bytes[pos..pos + 8]);
            u64::from_be_bytes(buf)
        } else {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&idx_bytes[pos..pos + 4]);
            u32::from_be_bytes(buf) as u64
        };
        pos += offset_size;
        let mut len_buf = [0u8; 4];
        len_buf.copy_from_slice(&idx_bytes[pos..pos + 4]);
        pos += 4;
        Some(Some((word, offset, u32::from_be_bytes(len_buf))))
    })
}

fn required_paths(
    file_paths: &[PathBuf],
) -> Result<(&PathBuf, &PathBuf, &PathBuf, Option<&PathBuf>)> {
    let by_suffix = index_by_suffix(file_paths);
    let ifo_path = by_suffix
        .get("ifo")
        .ok_or_else(|| ParserError::Validation("StarDict 词典缺少必要的 .ifo/.idx/.dict 文件".into()))?;
    let idx_path = by_suffix
        .get("idx")
        .or_else(|| by_suffix.get("idx_gz"))
        .ok_or_else(|| ParserError::Validation("StarDict 词典缺少必要的 .ifo/.idx/.dict 文件".into()))?;
    let dict_path = by_suffix
        .get("dict")
        .ok_or_else(|| ParserError::Validation("StarDict 词典缺少必要的 .ifo/.idx/.dict 文件".into()))?;
    Ok((ifo_path, idx_path, dict_path, by_suffix.get("syn").copied()))
}

impl super::DictionaryParser for StarDictParser {
    fn parse(
        &mut self,
        file_paths: &[PathBuf],
        _opts: &ParseOpts,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()> {
        let (ifo_path, _idx_path, _dict_path, _syn_path) = required_paths(file_paths)?;
        // opendict 按词典名前缀找文件；.ifo 的主干就是名字
        let dir = ifo_path
            .parent()
            .ok_or_else(|| ParserError::Validation(".ifo 路径没有父目录".into()))?;
        let name = ifo_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or_else(|| ParserError::Validation(".ifo 文件名无效".into()))?;
        // cache_to_disk=false：不往用户词典目录写解压缓存（Python 版无此副作用）
        let dict = opendict::stardict::StarDictDictionary::open_with_cache(dir, &name, false)
            .map_err(|e| ParserError::Internal(format!("StarDict 打开失败: {e}")))?;

        let mut batch: Vec<ParsedEntry> = Vec::with_capacity(2000);
        for i in 0..dict.entry_count() {
            let Some((word, raw)) = dict.raw_entry_at(i)? else { continue };
            // 对齐 Python _render_content：不分 sametypesequence 段，直接 UTF-8 解码
            let definition = String::from_utf8_lossy(raw).into_owned();
            batch.push(ParsedEntry::new(word, definition));
            if batch.len() >= 2000 {
                let out = std::mem::take(&mut batch);
                emit(out)?;
            }
        }
        // .syn 别名条目：复用目标词的释义，extra={"alias_of": 目标词}
        for i in 0..dict.synonym_count() {
            let Some((alias, target_index, target_word)) = dict.synonym_at(i) else { continue };
            let Some((_, raw)) = dict.raw_entry_at(target_index)? else { continue };
            let definition = String::from_utf8_lossy(raw).into_owned();
            let mut entry = ParsedEntry::new(alias, definition);
            entry.extra = Some(serde_json::json!({"alias_of": target_word}));
            batch.push(entry);
            if batch.len() >= 2000 {
                let out = std::mem::take(&mut batch);
                emit(out)?;
            }
        }
        if !batch.is_empty() {
            emit(batch)?;
        }
        Ok(())
    }

    fn sample(&mut self, file_paths: &[PathBuf], limit: usize) -> Result<Vec<ParsedEntry>> {
        let (ifo_path, idx_path, dict_path, _syn_path) = required_paths(file_paths)?;
        let meta = parse_ifo(ifo_path)?;
        let offset_bits: u32 = meta
            .get("idxoffsetbits")
            .and_then(|v| v.parse().ok())
            .unwrap_or(32);

        // 多取一些记录再过滤：开头可能有不少纯符号词头或无正文条目，都被 is_informative 剔掉
        let idx_bytes = read_idx_bytes(idx_path, Some(SAMPLE_IDX_BYTES))?;
        let mut entries: Vec<(String, u64, u32)> = Vec::new();
        for entry in iter_idx_entries(&idx_bytes, offset_bits) {
            match entry {
                Some(entry) => {
                    entries.push(entry);
                    if entries.len() >= limit * SAMPLE_SCAN_FACTOR {
                        break;
                    }
                }
                // 前缀末尾可能是半条记录，已取到的足够采样
                None => break,
            }
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        // 只读采样条目覆盖到的那段 .dict，并封顶
        let needed = entries
            .iter()
            .map(|(_, offset, length)| offset + *length as u64)
            .max()
            .unwrap_or(0)
            .min(SAMPLE_DICT_BYTES as u64) as usize;
        let dict_bytes = read_dict_content(dict_path, Some(needed))?;

        let mut sampled = Vec::new();
        for (word, offset, length) in entries {
            let start = offset as usize;
            let end = (start + length as usize).min(dict_bytes.len());
            if start > dict_bytes.len() {
                continue;
            }
            let definition = String::from_utf8_lossy(&dict_bytes[start..end]).into_owned();
            if !is_informative(&word, Some(&definition)) {
                continue;
            }
            sampled.push(ParsedEntry::new(word, definition));
            if sampled.len() >= limit {
                break;
            }
        }
        Ok(sampled)
    }

    fn sample_headwords(&mut self, file_paths: &[PathBuf], limit: usize) -> Result<Vec<String>> {
        let (ifo_path, idx_path, _dict_path, _syn_path) = required_paths(file_paths)?;
        let meta = parse_ifo(ifo_path)?;
        let offset_bits: u32 = meta
            .get("idxoffsetbits")
            .and_then(|v| v.parse().ok())
            .unwrap_or(32);
        let offset_size = if offset_bits == 64 { 8usize } else { 4 };
        // 一条记录里 \x00 之后还有 offset + length 两个字段，都要跳过才能到下一条词头起点
        let record_tail = 1 + offset_size + 4;

        let is_gz = is_gzip(idx_path, "gz");
        if is_gz {
            // .idx.gz 不能随机 seek；整份解压（封顶）后按步长取样
            let data = read_idx_bytes(idx_path, Some(SAMPLE_IDX_FULL_BYTES))?;
            let mut words = Vec::new();
            for entry in iter_idx_entries(&data, offset_bits) {
                let Some((word, _, _)) = entry else { break };
                if is_informative_headword(&word) {
                    words.push(word);
                }
            }
            return Ok(spread_downsample(words, limit));
        }

        let size = std::fs::metadata(idx_path)?.len() as usize;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut words = Vec::new();
        // 取样点数按 2 倍目标取，并且探完整段再统一下采样——中途收满就停会退回「只取开头」
        let probes = limit * 2;
        let mut file = std::fs::File::open(idx_path)?;
        for k in 0..probes {
            let target = std::cmp::min(size.saturating_sub(1), size * k / probes.max(1));
            use std::io::Seek;
            file.seek(std::io::SeekFrom::Start(target as u64))?;
            let mut chunk = vec![0u8; IDX_SEEK_CHUNK];
            let n = read_up_to(&mut file, &mut chunk)?;
            if n == 0 {
                continue;
            }
            chunk.truncate(n);
            // target 落在一条记录中间：先跳过当前被截断记录的尾部字段才是下一条起点
            let Some(zero) = chunk.iter().position(|&b| b == 0) else { continue };
            let start = zero + record_tail;
            let Some(end) = chunk[start..].iter().position(|&b| b == 0) else { continue };
            let end = start + end;
            let word = String::from_utf8_lossy(&chunk[start..end]).into_owned();
            if is_informative_headword(&word) {
                words.push(word);
            }
        }
        Ok(spread_downsample(words, limit))
    }
}

fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0usize;
    loop {
        let n = file.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
        if total == buf.len() {
            break;
        }
    }
    Ok(total)
}

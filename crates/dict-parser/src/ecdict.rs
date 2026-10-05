//! ECDICT CSV 解析器 —— 移植自 `app/parsers/ecdict.py`。

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::entry::{
    is_informative, is_informative_headword, spread_downsample, ParsedEntry, SAMPLE_SCAN_FACTOR,
};
use crate::{ParseOpts, ParserError, Result};

const EXTRA_INT_FIELDS: &[&str] = &["collins", "oxford", "bnc", "frq"];
const EXTRA_STR_FIELDS: &[&str] = &["pos", "tag", "exchange", "audio"];

pub struct EcdictParser;

fn open_csv_reader(
    path: &std::path::Path,
) -> Result<csv::Reader<std::io::BufReader<std::fs::File>>> {
    // utf-8-sig：去 BOM
    let file = std::fs::File::open(path)?;
    let mut buf_reader = std::io::BufReader::new(file);
    // 手动剥 BOM（csv crate 的 flexible 不处理 BOM）
    let mut probe = [0u8; 3];
    let n = buf_reader.read(&mut probe)? .min(3);
    if n == 3 && &probe == b"\xEF\xBB\xBF" {
        // BOM 已消费，回退 0
    } else {
        buf_reader.seek(SeekFrom::Start(0))?;
    }
    Ok(csv::Reader::from_reader(buf_reader))
}

fn row_get<'a>(headers: &[String], record: &'a csv::StringRecord, key: &str) -> Option<&'a str> {
    let index = headers.iter().position(|h| h == key)?;
    record.get(index)
}

fn to_extra_json(headers: &[String], record: &csv::StringRecord) -> Option<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for field in EXTRA_INT_FIELDS {
        let value = row_get(headers, record, field).unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        match value.parse::<i64>() {
            Ok(num) => {
                map.insert(field.to_string(), num.into());
            }
            Err(_) => {
                map.insert(field.to_string(), value.into());
            }
        }
    }
    for field in EXTRA_STR_FIELDS {
        let value = row_get(headers, record, field).unwrap_or("").trim();
        if !value.is_empty() {
            map.insert(field.to_string(), value.into());
        }
    }
    // detail 列若为合法 JSON 对象，透传合并进 extra
    let detail_raw = row_get(headers, record, "detail").unwrap_or("").trim();
    if !detail_raw.is_empty() {
        if let Ok(serde_json::Value::Object(detail)) = serde_json::from_str(detail_raw) {
            for (k, v) in detail {
                map.insert(k, v);
            }
        }
    }
    (!map.is_empty()).then(|| serde_json::Value::Object(map))
}

/// ECDICT 源 CSV 里同一单元格的多行释义用字面 "\n"（反斜杠+n）分隔，转成真换行
fn unescape_newlines(value: &str) -> String {
    value.replace("\\n", "\n")
}

impl super::DictionaryParser for EcdictParser {
    fn parse(
        &mut self,
        file_paths: &[PathBuf],
        _opts: &ParseOpts,
        emit: &mut dyn FnMut(Vec<ParsedEntry>) -> Result<()>,
    ) -> Result<()> {
        let csv_path = file_paths
            .first()
            .ok_or_else(|| ParserError::Validation("ECDICT 词典缺少 CSV 文件".into()))?;
        let mut reader = open_csv_reader(csv_path)?;
        let headers = reader
            .headers()
            .map_err(|e| ParserError::Internal(format!("读 CSV 表头失败: {e}")))?
            .clone();
        let headers: Vec<String> = headers.iter().map(|s| s.to_string()).collect();

        let mut batch: Vec<ParsedEntry> = Vec::with_capacity(2000);
        for record in reader.records() {
            let record =
                record.map_err(|e| ParserError::Internal(format!("读 CSV 行失败: {e}")))?;
            let word = row_get(&headers, &record, "word").unwrap_or("").trim();
            if word.is_empty() {
                continue;
            }
            let definition_en = row_get(&headers, &record, "definition")
                .unwrap_or("")
                .trim();
            let translation_zh = row_get(&headers, &record, "translation")
                .unwrap_or("")
                .trim();
            let en = unescape_newlines(definition_en);
            let zh = unescape_newlines(translation_zh);
            let combined = [en, zh]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut entry = ParsedEntry::new(word, combined);
            entry.phonetic = row_get(&headers, &record, "phonetic")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from);
            entry.extra = to_extra_json(&headers, &record);
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
        let csv_path = file_paths
            .first()
            .ok_or_else(|| ParserError::Validation("ECDICT 词典缺少 CSV 文件".into()))?;
        let mut reader = open_csv_reader(csv_path)?;
        let headers = reader
            .headers()
            .map_err(|e| ParserError::Internal(format!("读 CSV 表头失败: {e}")))?
            .clone();
        let headers: Vec<String> = headers.iter().map(|s| s.to_string()).collect();

        // 读到够 limit 条「有信息量」的词条；采样不需要 phonetic/extra（省 JSON 解析）
        let mut sampled = Vec::new();
        let mut scanned = 0usize;
        let scan_cap = limit * SAMPLE_SCAN_FACTOR;
        for record in reader.records() {
            scanned += 1;
            if scanned > scan_cap {
                break;
            }
            let record =
                record.map_err(|e| ParserError::Internal(format!("读 CSV 行失败: {e}")))?;
            let word = row_get(&headers, &record, "word").unwrap_or("").trim();
            if word.is_empty() {
                continue;
            }
            let definition_en = row_get(&headers, &record, "definition")
                .unwrap_or("")
                .trim();
            let translation_zh = row_get(&headers, &record, "translation")
                .unwrap_or("")
                .trim();
            let combined = [definition_en, translation_zh]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !is_informative(word, Some(&combined)) {
                continue;
            }
            sampled.push(ParsedEntry::new(word, combined));
            if sampled.len() >= limit {
                break;
            }
        }
        Ok(sampled)
    }

    fn sample_headwords(&mut self, file_paths: &[PathBuf], limit: usize) -> Result<Vec<String>> {
        let csv_path = file_paths
            .first()
            .ok_or_else(|| ParserError::Validation("ECDICT 词典缺少 CSV 文件".into()))?;
        let size = std::fs::metadata(csv_path)?.len() as usize;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut file = std::fs::File::open(csv_path)?;
        // 表头
        let mut header_line = String::new();
        file.read_to_string_off(&mut header_line)?;
        let word_index = csv::Reader::from_reader(header_line.as_bytes())
            .headers()
            .ok()
            .and_then(|h| h.iter().position(|h| h == "word"))
            .unwrap_or(0);

        // 按字节偏移跨整份取样；取样点 2 倍目标，探完整段再统一下采样
        let probes = limit * 2;
        let mut words = Vec::new();
        for k in 0..probes {
            let target = std::cmp::min(size.saturating_sub(1), size * k / probes.max(1));
            file.seek(SeekFrom::Start(target as u64))?;
            let mut discard = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut file), &mut discard);
            let mut line = String::new();
            let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(&mut file), &mut line);
            if line.is_empty() {
                continue;
            }
            let mut record = csv::StringRecord::new();
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_reader(line.as_bytes());
            if rdr.read_record(&mut record).ok() != Some(true) {
                continue;
            }
            if let Some(word) = record.get(word_index) {
                let word = word.trim();
                if is_informative_headword(word) {
                    words.push(word.to_string());
                }
            }
        }
        Ok(spread_downsample(words, limit))
    }
}

trait ReadToStringOff {
    fn read_to_string_off(&mut self, out: &mut String) -> std::io::Result<()>;
}

impl ReadToStringOff for std::fs::File {
    fn read_to_string_off(&mut self, out: &mut String) -> std::io::Result<()> {
        use std::io::BufRead;
        // 读一行（表头）
        let mut reader = std::io::BufReader::new(self.try_clone()?);
        let mut buf = Vec::new();
        reader.read_until(b'\n', &mut buf)?;
        // 去 BOM
        let slice = buf.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&buf);
        out.push_str(&String::from_utf8_lossy(slice));
        Ok(())
    }
}

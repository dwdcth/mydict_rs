//! MDX 头部探测：读前几个字节判定引擎版本，供 v1/v2（mdictlib）与 v3（opendict）路由。
//!
//! MDX 头部布局：4 字节大端长度 + UTF-16LE 的 XML 属性串 + 4 字节 adler32（小端）。
//! 头加密（Salsa20）的文件探测会失败 → 返回 None，由调用方先试 mdictlib。

use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdictFamily {
    /// GeneratedByEngineVersion < 3.0 → mdictlib（v1.2 + v2.0）
    Mdictlib,
    /// GeneratedByEngineVersion >= 3.0 → opendict-rs（v3）
    Opendict,
}

/// 读 MDX 头解析 `GeneratedByEngineVersion`；文件不可读/头畸形/非 UTF-16 → None
pub fn probe_family(mdx_path: &Path) -> Option<MdictFamily> {
    let mut file = File::open(mdx_path).ok()?;
    let mut len_buf = [0u8; 4];
    file.read_exact(&mut len_buf).ok()?;
    let header_len = u32::from_be_bytes(len_buf) as usize;
    // 防御：头长度异常按探测失败处理（真实头从几十字节到几十 KB）
    if header_len == 0 || header_len > 16 * 1024 * 1024 {
        return None;
    }
    let mut header_bytes = vec![0u8; header_len];
    file.read_exact(&mut header_bytes).ok()?;
    if header_bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = header_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let version = engine_version_of(&text)?;
    if version >= 3.0 {
        Some(MdictFamily::Opendict)
    } else {
        Some(MdictFamily::Mdictlib)
    }
}

/// 从头 XML 串里抽 GeneratedByEngineVersion 的数值（与 opendict header.rs 同一判据）
fn engine_version_of(header: &str) -> Option<f32> {
    let key = "GeneratedByEngineVersion";
    let start = header.find(key)?;
    let rest = &header[start + key.len()..];
    let eq = rest.find('=')?;
    let rest = &rest[eq + 1..];
    let rest = rest.trim_start();
    let quote = rest.chars().next()?;
    let value: String = if quote == '"' || quote == '\'' {
        let inner = &rest[1..];
        let end = inner.find(quote)?;
        inner[..end].to_string()
    } else {
        rest.split(['>', ' ', '\n']).next()?.to_string()
    };
    value.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(
            engine_version_of(r#"<Dict GeneratedByEngineVersion="2.0" Encoding="UTF-8">"#),
            Some(2.0)
        );
        assert_eq!(
            engine_version_of(r#"<Dict GeneratedByEngineVersion="3.0" >"#),
            Some(3.0)
        );
        assert_eq!(
            engine_version_of("<Dict GeneratedByEngineVersion='1.2'>"),
            Some(1.2)
        );
        assert_eq!(engine_version_of("<Dict Title='x'>"), None);
    }
}

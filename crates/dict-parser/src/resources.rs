//! 资源落盘 / 引用改写 / 大小写不敏感解析 —— 移植自 `app/services/resource_service.py`。

use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// 词条文档（含它引用的 CSS）可能用到的资源类型。带点的全小写形式，上传白名单也复用它。
///
/// 刻意不含 Eudic 专有索引（.db / .db-wal / .bix / .bin）与 .pdf：实测词典目录里这类文件
/// 占了附属文件总大小的六成，却与 HTML 渲染毫无关系。
pub const SIBLING_RESOURCE_EXTENSIONS: &[&str] = &[
    ".css", ".js", ".mjs", ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".avif", ".bmp",
    ".ico", ".cur", ".woff", ".woff2", ".ttf", ".otf", ".eot", ".mp3", ".wav", ".ogg", ".oga",
    ".opus", ".m4a", ".aac", ".flac", ".spx", ".mp4", ".webm", ".html", ".htm", ".txt", ".json",
    ".xml", ".ini",
];

/// /dict-res 响应的 Content-Type。不交给 mimetypes 猜：容器里常常没有 /etc/mime.types，
/// 内置表不认 .woff2/.otf/.ogg/.webp/.spx 等；而响应带了 nosniff，Chrome 的 ORB 会拦掉
/// `<img>`/`<audio>` 拿到的 text/plain，资源就静默失效了。
pub fn resource_media_type(path: &Path) -> &'static str {
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    media_type_for_suffix(&suffix)
}

pub fn media_type_for_suffix(suffix: &str) -> &'static str {
    match suffix {
        ".css" => "text/css",
        ".js" | ".mjs" => "text/javascript",
        // 词典里的 .ini 是以 <script src="config.ini"> 加载的 JS 配置（The Little Dict
        // 的发音、板块开关），带 nosniff 时不是 JS 类型浏览器会拒绝执行
        ".ini" => "text/javascript",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".svg" => "image/svg+xml",
        ".webp" => "image/webp",
        ".avif" => "image/avif",
        ".bmp" => "image/bmp",
        ".ico" | ".cur" => "image/x-icon",
        ".tif" | ".tiff" => "image/tiff",
        ".woff" => "font/woff",
        ".woff2" => "font/woff2",
        ".ttf" => "font/ttf",
        ".otf" => "font/otf",
        ".eot" => "application/vnd.ms-fontobject",
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".ogg" | ".oga" | ".opus" | ".spx" => "audio/ogg",
        ".m4a" => "audio/mp4",
        ".aac" => "audio/aac",
        ".flac" => "audio/flac",
        ".mp4" => "video/mp4",
        ".webm" => "video/webm",
        ".html" | ".htm" => "text/html",
        ".txt" => "text/plain",
        ".json" => "application/json",
        ".xml" => "text/xml",
        // 不认识的一律 octet-stream，交给浏览器按内容嗅探（.mdd 里常有无扩展名图片）
        _ => "application/octet-stream",
    }
}

/// 匹配 HTML 中 src="..." / href="..." 属性值。
///
/// Python 版用 (?P=quote) 反向引用保证引号配对，regex crate 不支持反向引用，
/// 改成单/双引号两个分支（语义等价）。
static RESOURCE_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?P<attr>\b(?:src|href)\s*=\s*)(?:"(?P<path_d>[^"]+)"|'(?P<path_s>[^']+)')"#,
    )
    .unwrap()
});

/// 这些前缀开头的引用原样保留：entry:// 词条内跳转（前端拦截）、# 锚点、// 与 www. 外链、
/// javascript:/file: 一律不动（前端会拦截点击，绝不把它改成可执行的样子）。
static SKIP_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:https?://|data:|entry://|#|//|www\.|mailto:|javascript:|ftp://|blob:|tel:)")
        .unwrap()
});

/// sound:// 是 MDict 的音频引用协议，指向 .mdd 里解包出来的音频文件
static SOUND_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^sound://").unwrap());

/// file:///… 是 MDict 里「词典资源根目录」的写法，与 sound:// 同类。必须放在通用 scheme
/// 判断之前。`file:` 后跟两三个斜杠、正反斜杠都有实例（朗文 `file://media/...`、
/// 汉典 `file:///down/...`），一律吃掉。
static FILE_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^file:[\\/]+").unwrap());

/// 「带 scheme」的通用判据：字母开头 + 若干合法字符 + 冒号（放最后，未知协议保守留下）
static SCHEME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.\-]*:").unwrap());

/// 把词典内部的资源相对路径规范化为正斜杠、去掉开头分隔符，拒绝路径穿越。
pub fn normalize_resource_path(raw_path: &str) -> Result<String, crate::ParserError> {
    let path = raw_path.replace('\\', "/");
    let path = path.trim_start_matches('/');
    let parts: Vec<&str> = path
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.iter().any(|p| *p == "..") {
        return Err(crate::ParserError::Validation(format!(
            "resource path traversal rejected: {raw_path:?}"
        )));
    }
    Ok(parts.join("/"))
}

/// 去掉历史坏链接里多出来的一截 `file:/` 前缀（早期改写 bug 的入库值兼容）。
pub fn strip_legacy_file_prefix(relative: &str) -> String {
    FILE_PREFIX_RE.replace(relative, "").into_owned()
}

/// 将资源内容写入 resource_dir/relative_path，自动创建父目录。
///
/// overwrite=false 用于给**正在服务**的词典补文件：已存在的跳过，新文件走
/// 「临时文件 + rename」，不让并发请求读到半截内容。
pub fn write_resource(
    resource_dir: &Path,
    relative_path: &str,
    content: &[u8],
    overwrite: bool,
) -> Result<(), crate::ParserError> {
    let normalized = normalize_resource_path(relative_path)?;
    if normalized.is_empty() {
        return Ok(());
    }
    let target = resource_dir.join(&normalized);
    if !overwrite && target.exists() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if overwrite {
        std::fs::write(&target, content)?;
        return Ok(());
    }
    let temporary = target.with_file_name(format!(
        "{}.tmp-{}",
        target.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        std::process::id()
    ));
    match std::fs::write(&temporary, content)
        .and_then(|_| std::fs::rename(&temporary, &target))
    {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temporary);
            Err(err.into())
        }
    }
}

/// 把「词典相关路径」统一成要扫描的目录列表（文件取父目录，目录原样，去重保序）
fn source_dirs(sources: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for source in sources {
        let directory = if source.is_dir() {
            source.clone()
        } else {
            source.parent().map(|p| p.to_path_buf()).unwrap_or_default()
        };
        if !dirs.contains(&directory) {
            dirs.push(directory);
        }
    }
    dirs
}

/// 超过这个条目数就不建索引（The little dict 的 res/ 顶层有 67.6 万个文件，建索引要几十 MB）。
/// 超限时返回空索引且结果照样被缓存，代价只在第一次付出。
const MAX_INDEXED_NAMES: usize = 30_000;
const DIRECTORY_INDEX_CACHE: usize = 16;

struct DirectoryIndexCache {
    // 简单 LRU：HashMap + 访问序链表太重，这里 16 个目录用 Vec LRU 足够
    entries: Vec<(PathBuf, HashMap<String, String>)>,
}

static DIRECTORY_INDEX: LazyLock<Mutex<DirectoryIndexCache>> = LazyLock::new(|| {
    Mutex::new(DirectoryIndexCache {
        entries: Vec::new(),
    })
});

/// 目录项「小写名 → 真实名」，供大小写不敏感兜底查找用
fn directory_index(directory: &Path) -> HashMap<String, String> {
    let mut cache = DIRECTORY_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = cache.entries.iter().position(|(dir, _)| dir == directory) {
        // 移到末尾 = 最近使用
        let (_, index) = cache.entries.remove(pos);
        cache.entries.push((directory.to_path_buf(), index.clone()));
        return index;
    }
    let mut index = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            if index.len() >= MAX_INDEXED_NAMES {
                index.clear();
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            index.insert(name.to_lowercase(), name);
        }
    }
    if cache.entries.len() >= DIRECTORY_INDEX_CACHE {
        cache.entries.remove(0);
    }
    cache
        .entries
        .push((directory.to_path_buf(), index.clone()));
    index
}

/// 在 root 下逐段按大小写不敏感查找 relative；找到文件才返回。
///
/// 词典大多在 Windows 上打包，词条引用常与 .mdd 里的键大小写不一致（汉典的图片引用
/// 全部小写、实际键混合大小写）。逐段解析，每段仍优先精确匹配。
fn match_case_insensitively(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        let candidate = current.join(part);
        if candidate.exists() {
            current = candidate;
            continue;
        }
        let real = directory_index(&current).get(&part.to_lowercase())?.clone();
        current = current.join(real);
    }
    current.is_file().then_some(current)
}

/// 把资源相对路径解析成 `res/` 下的真实文件，解析不到返回 None。
/// 调用方需先做路径穿越校验、并去掉历史坏链接前缀。
pub fn resolve_resource_file(res_dir: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let target = res_dir.join(relative);
    if target.is_file() {
        return Some(target);
    }
    match_case_insensitively(res_dir, relative)
}

/// 把词典文件旁边的附属资源复制进 `resource_dir`，返回成功复制的文件数。
///
/// MDict 的惯例是把样式表、字体、脚本、图片放在 `.mdx` 同级目录（实测大辞泉的 oxbw.css、
/// 広辞苑的 6 个 .otf 都是如此，Weblio 甚至没有 .mdd），只解包 .mdd 会让它们全部 404。
/// 只扫一层目录、只复制白名单扩展名的直接子文件；已存在的不覆盖（.mdd 解出的更权威）；
/// 临时文件 + rename 原子替换。
pub fn copy_sibling_resources(resource_dir: &Path, sources: &[PathBuf]) -> usize {
    let mut count = 0usize;
    for directory in source_dirs(sources) {
        let mut children: Vec<PathBuf> = match std::fs::read_dir(&directory) {
            Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
            Err(err) => {
                tracing::warn!(dir = %directory.display(), error = %err, "无法读取词典目录，跳过附属资源复制");
                continue;
            }
        };
        children.sort_by_key(|item| {
            item.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });

        for child in children {
            let suffix = child
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            if !SIBLING_RESOURCE_EXTENSIONS.contains(&suffix.as_str()) {
                continue;
            }
            // 符号链接可能指向词典目录之外，不跟着走
            if child.is_symlink() || !child.is_file() {
                continue;
            }
            let Some(name) = child.file_name() else { continue };
            let name = name.to_string_lossy().to_string();
            let target = resource_dir.join(&name);
            if target.exists() {
                continue;
            }
            let temporary = resource_dir.join(format!(
                "{}.tmp-{}",
                name,
                std::process::id()
            ));
            let copied = (|| -> std::io::Result<()> {
                std::fs::create_dir_all(resource_dir)?;
                std::fs::copy(&child, &temporary)?;
                std::fs::rename(&temporary, &target)?;
                Ok(())
            })();
            match copied {
                Ok(()) => count += 1,
                Err(err) => {
                    tracing::warn!(file = %child.display(), error = %err, "复制附属资源失败");
                    let _ = std::fs::remove_file(&temporary);
                }
            }
        }
    }
    count
}

/// percent-encode（URL 路径段），对齐 Python urllib.parse.quote 的默认安全集 "/"
fn quote_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric()
            || matches!(c, '/' | '.' | '_' | '-' | '~')
        {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 找出 `.mdx` **同名**的 `.css`/`.js` 附属文件（MDict 客户端会自动加载它们，
/// 词条 HTML 里从不引用），返回 `(文件名, /dict-res URL)` 列表。
pub fn same_name_assets(
    res_dir: &Path,
    dictionary_id: i32,
    source_file: &str,
) -> Vec<(String, String)> {
    let stem = Path::new(source_file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut assets = Vec::new();
    for extension in [".css", ".js"] {
        let name = format!("{stem}{extension}");
        if resolve_resource_file(res_dir, &name).is_none() {
            continue;
        }
        assets.push((
            name.clone(),
            format!("/dict-res/{dictionary_id}/res/{}", quote_path(&name)),
        ));
    }
    assets
}

/// 把 ?query 与 #fragment 从路径里拆出来，返回 (路径, 后缀)
fn split_suffix(raw: &str) -> (&str, String) {
    let (head, fragment) = match raw.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (raw, None),
    };
    let (path, query) = match head.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (head, None),
    };
    let mut suffix = String::new();
    if let Some(query) = query {
        suffix.push('?');
        suffix.push_str(query);
    }
    if let Some(fragment) = fragment {
        suffix.push('#');
        suffix.push_str(fragment);
    }
    (path, suffix)
}

fn to_resource_url(raw: &str, dictionary_id: i32) -> Option<String> {
    let (path, suffix) = split_suffix(raw);
    let normalized = normalize_resource_path(path).ok()?;
    if normalized.is_empty() {
        return None;
    }
    Some(format!(
        "/dict-res/{dictionary_id}/res/{normalized}{suffix}"
    ))
}

/// 单条属性值的改写规则；返回 None 表示「不改写」。
fn rewrite_value(raw: &str, dictionary_id: i32) -> Option<String> {
    // file:///down/x.gif 指的是词典资源根目录下的 down/x.gif（7 部词典在用）
    if FILE_PREFIX_RE.is_match(raw) {
        let stripped = FILE_PREFIX_RE.replace(raw, "").into_owned();
        return Some(to_resource_url(&stripped, dictionary_id).unwrap_or_else(|| raw.to_string()));
    }
    if SKIP_PREFIX_RE.is_match(raw) {
        return None;
    }
    // sound://audio/x.spx -> /dict-res/{id}/res/audio/x.spx，前端据此播放
    if SOUND_PREFIX_RE.is_match(raw) {
        let stripped = SOUND_PREFIX_RE.replace(raw, "").into_owned();
        return Some(to_resource_url(&stripped, dictionary_id).unwrap_or_else(|| raw.to_string()));
    }
    // 其余任何带 scheme 的引用都不属于「词典内部资源」，保守留下
    if SCHEME_RE.is_match(raw) {
        return None;
    }
    // 无扩展名的裸相对路径基本都是词条链接（entry:// 的简写），改了只会 404，不动
    let head = raw.split('#').next().unwrap_or("").split('?').next().unwrap_or("");
    let last_seg = head.rsplit('/').next().unwrap_or(head);
    if !last_seg.contains('.') {
        return None;
    }
    Some(to_resource_url(raw, dictionary_id).unwrap_or_else(|| raw.to_string()))
}

/// 把释义 HTML 中的相对资源引用改写为 /dict-res/{dictionary_id}/res/... 绝对 URL。
/// entry:// 词条链接与外部链接原样保留。
pub fn rewrite_resource_refs(html: &str, dictionary_id: i32) -> String {
    RESOURCE_REF_RE
        .replace_all(html, |caps: &regex::Captures| {
            let attr = caps.name("attr").map(|m| m.as_str()).unwrap_or_default();
            let value = caps
                .name("path_d")
                .or_else(|| caps.name("path_s"))
                .map(|m| m.as_str())
                .unwrap_or_default();
            match rewrite_value(value, dictionary_id) {
                Some(rewritten) if rewritten != value => {
                    let quote = if caps.name("path_d").is_some() { '"' } else { '\'' };
                    format!("{attr}{quote}{rewritten}{quote}")
                }
                _ => caps[0].to_string(),
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(html: &str) -> String {
        rewrite_resource_refs(html, 7)
    }

    #[test]
    fn normalize_rejects_traversal() {
        assert_eq!(
            normalize_resource_path(r"\a\b\c.png").unwrap(),
            "a/b/c.png"
        );
        assert_eq!(normalize_resource_path("/a//./b").unwrap(), "a/b");
        assert!(normalize_resource_path("../etc/passwd").is_err());
        assert!(normalize_resource_path(r"a\..\b").is_err());
    }

    #[test]
    fn rewrite_keeps_external_and_entry_refs() {
        assert_eq!(
            rewrite(r#"<a href="https://x.com/a">t</a><img src="data:image/png;base64,xx">"#),
            r#"<a href="https://x.com/a">t</a><img src="data:image/png;base64,xx">"#
        );
        assert_eq!(
            rewrite(r#"<a href="entry://苹果">x</a>"#),
            r#"<a href="entry://苹果">x</a>"#
        );
        assert_eq!(
            rewrite(r##"<a href="#sec">x</a><a href="//cdn.com/a.js">x</a><a href="www.a.com">x</a>"##),
            r##"<a href="#sec">x</a><a href="//cdn.com/a.js">x</a><a href="www.a.com">x</a>"##
        );
        assert_eq!(
            rewrite(r#"<a href="javascript:alert(1)">x</a><a href="mailto:a@b.c">x</a>"#),
            r#"<a href="javascript:alert(1)">x</a><a href="mailto:a@b.c">x</a>"#
        );
    }

    #[test]
    fn rewrite_converts_internal_refs() {
        assert_eq!(
            rewrite(r#"<img src="img/logo.png">"#),
            r#"<img src="/dict-res/7/res/img/logo.png">"#
        );
        assert_eq!(
            rewrite(r#"<a href='sound://audio/x.spx'>p</a>"#),
            r#"<a href='/dict-res/7/res/audio/x.spx'>p</a>"#
        );
        assert_eq!(
            rewrite(r#"<img src="file:///down/x.gif">"#),
            r#"<img src="/dict-res/7/res/down/x.gif">"#
        );
        assert_eq!(
            rewrite(r#"<img src="file://media/a.png">"#),
            r#"<img src="/dict-res/7/res/media/a.png">"#
        );
        // 查询串/锚点保留在尾部
        assert_eq!(
            rewrite(r#"<img src="a.png?v=2#frag">"#),
            r#"<img src="/dict-res/7/res/a.png?v=2#frag">"#
        );
        // 未知协议保守留下
        assert_eq!(
            rewrite(r#"<a href="ws://x">y</a>"#),
            r#"<a href="ws://x">y</a>"#
        );
        // 裸相对路径无扩展名（词条链接简写）不动
        assert_eq!(
            rewrite(r#"<a href="apple">see</a>"#),
            r#"<a href="apple">see</a>"#
        );
    }

    #[test]
    fn legacy_file_prefix_stripped() {
        assert_eq!(strip_legacy_file_prefix("file:/down/x.gif"), "down/x.gif");
        assert_eq!(strip_legacy_file_prefix("file:///down/x.gif"), "down/x.gif");
        assert_eq!(strip_legacy_file_prefix("normal/x.gif"), "normal/x.gif");
    }

    #[test]
    fn write_resource_atomic_and_skip_existing() {
        let dir = tempfile::tempdir().unwrap();
        write_resource(dir.path(), "a/b/c.css", b"body{}", true).unwrap();
        assert_eq!(std::fs::read(dir.path().join("a/b/c.css")).unwrap(), b"body{}");
        // overwrite=false：已存在跳过
        write_resource(dir.path(), "a/b/c.css", b"CHANGED", false).unwrap();
        assert_eq!(std::fs::read(dir.path().join("a/b/c.css")).unwrap(), b"body{}");
        // overwrite=false：新文件写入
        write_resource(dir.path(), "a/b/d.css", b"new", false).unwrap();
        assert_eq!(std::fs::read(dir.path().join("a/b/d.css")).unwrap(), b"new");
        // 无 .tmp- 残留
        for entry in std::fs::read_dir(dir.path().join("a/b")).unwrap().flatten() {
            assert!(!entry.file_name().to_string_lossy().contains(".tmp-"));
        }
    }

    #[test]
    fn case_insensitive_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Images")).unwrap();
        std::fs::write(dir.path().join("Images/Logo.PNG"), b"x").unwrap();
        // 精确
        assert!(resolve_resource_file(dir.path(), "Images/Logo.PNG").is_some());
        // 大小写不敏感兜底
        assert!(resolve_resource_file(dir.path(), "images/logo.png").is_some());
        assert!(resolve_resource_file(dir.path(), "IMAGES/LOGO.PNG").is_some());
        assert!(resolve_resource_file(dir.path(), "images/missing.png").is_none());
    }

    #[test]
    fn sibling_copy_filters_and_preserves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("style.css"), b"css").unwrap();
        std::fs::write(src.path().join("font.woff2"), b"font").unwrap();
        std::fs::write(src.path().join("data.db"), b"db").unwrap(); // 白名单外
        std::fs::write(src.path().join("dict.mdx"), b"mdx").unwrap(); // 本体
        let copied = copy_sibling_resources(dst.path(), &[src.path().to_path_buf()]);
        assert_eq!(copied, 2);
        assert!(dst.path().join("style.css").exists());
        assert!(dst.path().join("font.woff2").exists());
        assert!(!dst.path().join("data.db").exists());
        // 再跑一次：已存在不覆盖，计数 0
        std::fs::write(src.path().join("style.css"), b"CHANGED").unwrap();
        assert_eq!(copy_sibling_resources(dst.path(), &[src.path().to_path_buf()]), 0);
        assert_eq!(std::fs::read(dst.path().join("style.css")).unwrap(), b"css");
    }
}

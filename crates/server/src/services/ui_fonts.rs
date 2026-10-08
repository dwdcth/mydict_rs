//! 聚合启用词典 CSS 里的 @font-face，给前端 UI（搜索框、历史列表等）
//! 提供 PUA 生僻字字形的显示兜底。
//!
//! 词条 iframe 里词典 CSS 自带字体（说文系的 `FZ-Zhuan.woff` 覆盖
//! U+F0000-FFFFF 的古文字字形），但父页面 UI 引用不到它们——PUA 字头在
//! 搜索框里就是方框。做法：扫各启用词典样本词条引用的 CSS，抽出
//! @font-face 规则，资源地址改写成 `/dict-res` 路由、字体统一改名
//! `MydictDictFont`；前端把它挂在字体栈末尾，浏览器按 unicode-range
//! 逐字回退（命不中不下载，无额外开销）。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use moka::sync::Cache as MokaCache;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use tokio::sync::OnceCell;

use crate::AppState;

/// 前端字体栈里挂的兜底字体名（typography.css 同名引用）
pub const UI_FONT_FAMILY: &str = "MydictDictFont";
const FONT_EXTS: &[&str] = &["woff2", "woff", "ttf", "otf"];
/// 聚合结果缓存（词典增删后至多 10 分钟生效）
static CSS_CACHE: OnceCell<MokaCache<(), Arc<String>>> = OnceCell::const_new();

pub async fn ui_fonts_css(app: &AppState) -> Arc<String> {
    let cache = CSS_CACHE
        .get_or_init(|| async {
            MokaCache::builder()
                .time_to_live(std::time::Duration::from_secs(600))
                .max_capacity(1)
                .build()
        })
        .await;
    if let Some(hit) = cache.get(&()) {
        return hit;
    }
    let css = Arc::new(assemble(app).await);
    cache.insert((), css.clone());
    css
}

async fn assemble(app: &AppState) -> String {
    let mut out = String::from("/* mydict 词典字体兜底（按启用词典自动聚合，勿手改） */\n");
    let mut seen_urls = BTreeSet::new();
    let mut ranged_count = 0usize;
    for dict_id in enabled_dictionary_ids(&app.db).await {
        for href in sample_entry_css_links(&app.db, dict_id).await {
            let Some(content) = fetch_resource_text(app, dict_id, &href).await else {
                continue;
            };
            for face in extract_font_faces(&content, dict_id) {
                if !seen_urls.insert(face.url.clone()) {
                    continue;
                }
                let range = face
                    .unicode_range
                    .as_deref()
                    .filter(|r| !r.trim().is_empty())
                    .map(|r| format!(";unicode-range:{r}"))
                    .unwrap_or_default();
                let format = face
                    .format
                    .as_deref()
                    .filter(|f| !f.trim().is_empty())
                    .map(|f| format!(" format('{f}')"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "@font-face{{font-family:'{UI_FONT_FAMILY}';src:url({}){}{}}}\n",
                    face.url, format, range
                ));
                if face.unicode_range.is_some() {
                    ranged_count += 1;
                }
            }
        }
    }
    // 同族多脸时 Chrome 按声明序取第一张 unicode-range 命中的脸；「无范围」
    // 的脸等于全匹配，放在前面会把 PUA 字形劫走（选中后缺字形直接 .notdef，
    // 不会继续试后面的脸）。这里把有范围的脸挪到前面、无范围的垫底。
    reorder_ranged_first(&mut out, ranged_count);
    out
}

/// 把 CSS 文本里有 unicode-range 的 @font-face 规则挪到无范围的之前
fn reorder_ranged_first(css: &mut String, expected_ranged: usize) {
    if expected_ranged == 0 {
        return;
    }
    let rules: Vec<String> = css
        .lines()
        .filter(|l| l.starts_with("@font-face"))
        .map(str::to_string)
        .collect();
    let mut ranged: Vec<&String> = rules.iter().filter(|r| r.contains("unicode-range")).collect();
    let mut bare: Vec<&String> = rules.iter().filter(|r| !r.contains("unicode-range")).collect();
    let mut ordered: Vec<&String> = Vec::with_capacity(rules.len());
    ordered.append(&mut ranged);
    ordered.append(&mut bare);
    let head_len = css.find("@font-face").unwrap_or(css.len());
    let head = css[..head_len].to_string();
    let joined: Vec<&str> = ordered.iter().map(|s| s.as_str()).collect();
    *css = head + &joined.join("\n") + "\n";
}

async fn enabled_dictionary_ids(db: &DatabaseConnection) -> Vec<i32> {
    db.query_all_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "SELECT id FROM dictionaries WHERE status = 'enabled' ORDER BY id",
        [],
    ))
    .await
    .ok()
    .into_iter()
    .flatten()
    .filter_map(|r| r.try_get::<i32>("", "id").ok())
    .collect()
}

/// 样本词条的释义里引用的 CSS 地址（导入期已改写成 /dict-res/{id}/res/…）
async fn sample_entry_css_links(db: &DatabaseConnection, dict_id: i32) -> Vec<String> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT definition FROM dict_entries WHERE dictionary_id = $1 AND definition LIKE '%.css%' LIMIT 10",
            [dict_id.into()],
        ))
        .await
        .unwrap_or_default();
    let prefix = format!("/dict-res/{dict_id}/res/");
    let mut links = Vec::new();
    for row in rows {
        let Ok(def) = row.try_get::<String>("", "definition") else {
            continue;
        };
        let mut from = 0usize;
        while let Some(rel) = def[from..].find(&prefix) {
            let start = from + rel;
            let rest = &def[start + prefix.len()..];
            let end = rest
                .find(['"', '\'', '>', ' ', ')'])
                .unwrap_or(rest.len());
            let url = format!("{prefix}{}", &rest[..end]);
            if url.to_lowercase().ends_with(".css") && !links.contains(&url) {
                links.push(url);
                if links.len() >= 4 {
                    return links;
                }
            }
            from = start + prefix.len() + end;
        }
    }
    links
}

/// 取一个资源的文本内容（CSS）：先磁盘 res/，再 .mdd 按需读
async fn fetch_resource_text(app: &AppState, dict_id: i32, href: &str) -> Option<String> {
    let prefix = format!("/dict-res/{dict_id}/res/");
    let rel = href.strip_prefix(&prefix)?;
    let normalized =
        dict_parser::resources::normalize_resource_path(rel).ok()?;
    let normalized =
        dict_parser::resources::strip_legacy_file_prefix(&normalized);
    let res_dir = PathBuf::from(&app.cfg.dictionary_storage_path)
        .join(dict_id.to_string())
        .join("res");

    // 磁盘（大小写不敏感 + 候选链；放阻塞线程，与 /dict-res 同口径）
    let candidates = dict_parser::resources::resource_candidates(&normalized);
    let res_dir2 = res_dir.clone();
    let disk = tokio::task::spawn_blocking(move || {
        candidates
            .iter()
            .find_map(|c| dict_parser::resources::resolve_resource_file(&res_dir2, c))
    })
    .await
    .ok()
    .flatten();
    if let Some(path) = disk {
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    // .mdd
    crate::services::mdd_resources::lookup_resource(app, dict_id, &normalized)
        .await
        .map(|b| String::from_utf8_lossy(b.as_slice()).into_owned())
}

struct FontFace {
    url: String,
    format: Option<String>,
    unicode_range: Option<String>,
}

/// 从 CSS 文本里抽 @font-face：src 里第一个字体扩展的 url() 改写成
/// `/dict-res` 绝对地址；只有 local() 的规则（本机装了才有效）跳过
fn extract_font_faces(css: &str, dict_id: i32) -> Vec<FontFace> {
    let mut faces = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = css[from..].find("@font-face") {
        let head = from + rel;
        let Some(open) = css[head..].find('{') else { break };
        let Some(close) = css[head + open..].find('}') else { break };
        let block = &css[head + open + 1..head + open + close];
        from = head + open + close;

        let src = descriptor(block, "src").unwrap_or_default();
        let url = first_font_url(&src, dict_id);
        let Some(url) = url else { continue };
        faces.push(FontFace {
            url,
            format: descriptor(block, "src")
                .and_then(|_| extract_format(&src)),
            // 词典 CSS 里偶见 en/em dash 写区间（U+E000–F8FF），浏览器解析不了
            unicode_range: descriptor(block, "unicode-range")
                .map(|r| r.replace(['–', '—', '‒'], "-")),
        });
    }
    faces
}

/// 取块内某个 descriptor 的值（到分号或块尾）
fn descriptor(block: &str, name: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(rel) = block[from..].find(name) {
        let after = &block[from + rel + name.len()..];
        // 必须紧跟冒号（避免 "src" 命中 "src-local" 之类）
        let colon = after.trim_start();
        let Some(rest) = colon.strip_prefix(':') else {
            from += rel + name.len();
            continue;
        };
        let value = &rest[..rest.find(';').unwrap_or(rest.len())];
        return Some(value.trim().to_string());
    }
    None
}

fn first_font_url(src: &str, dict_id: i32) -> Option<String> {
    let mut from = 0usize;
    while let Some(rel) = src[from..].find("url(") {
        let start = from + rel + 4;
        let rest = &src[start..];
        let Some(end) = rest.find(')') else { break };
        let raw = rest[..end].trim().trim_matches(|c| c == '"' || c == '\'');
        from = start + end;
        if raw.starts_with("data:") {
            return Some(raw.to_string());
        }
        if raw.starts_with("http://") || raw.starts_with("https://") {
            continue; // 在线字体离线拿不到
        }
        let lower = raw.to_lowercase();
        if !FONT_EXTS.iter().any(|e| lower.ends_with(&format!(".{e}"))) {
            continue;
        }
        let rel_path = raw.trim_start_matches("./").trim_start_matches('/');
        let quoted = dict_parser::resources::quote_path_pub(rel_path);
        return Some(format!("/dict-res/{dict_id}/res/{quoted}"));
    }
    None
}

fn extract_format(src: &str) -> Option<String> {
    let rel = src.find("format(")?;
    let rest = &src[rel + 7..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().trim_matches(|c| c == '"' || c == '\'').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_rewrites_font_faces() {
        let css = r#"
        @font-face {font-family:FSung-SW-super; src:url('./FSung-SW-super.woff') format('woff')}
        @font-face {font-family:FZ-Zhuan; src:url('./FZ-Zhuan.woff') format('woff'); unicode-range:U+F0000-FFFFF;}
        @font-face {font-family:FSung-F; src:local('FSung-F')}
        body {color:red}
        "#;
        let faces = extract_font_faces(css, 5);
        assert_eq!(faces.len(), 2, "local-only 面应被跳过");
        assert_eq!(faces[0].url, "/dict-res/5/res/FSung-SW-super.woff");
        assert_eq!(faces[0].format.as_deref(), Some("woff"));
        assert!(faces[0].unicode_range.is_none());
        assert_eq!(
            faces[1].unicode_range.as_deref(),
            Some("U+F0000-FFFFF"),
            "unicode-range 必须保留，否则兜底字体会抢普通字的渲染"
        );
    }

    #[test]
    fn descriptor_ignores_partial_name_matches() {
        let block = "font-family:X; src:url(a.woff); unicode-range:U+1-2";
        assert_eq!(descriptor(block, "src").as_deref(), Some("url(a.woff)"));
        assert_eq!(descriptor(block, "unicode-range").as_deref(), Some("U+1-2"));
    }
}

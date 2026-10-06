//! 词条文档渲染 —— 移植自 `app/services/entry_render_service.py`。
//!
//! 把词条释义包成一个可在 iframe 里安全渲染的独立 HTML 文档（sandbox 只给
//! allow-scripts、不加 allow-same-origin）：既保留各词典自己的 CSS/JS，又拿不到
//! 父页面的任何东西（token 在 localStorage）。

use regex::Regex;
use std::sync::LazyLock;

static DOCTYPE_OR_HTML_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*<(?:!doctype|html)\b").unwrap());
static HEAD_OPEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<head\b[^>]*>").unwrap());
static HTML_OPEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<html\b[^>]*>").unwrap());

const SANDBOX_CSP: &str = "sandbox allow-scripts";

/// 引导脚本（include_str! 编进二进制，只读一次的语义天然成立）
const BOOTSTRAP_SOURCE: &str = include_str!("iframe_bootstrap.js");

/// 暗色适配样式：只翻「明确写着黑/白」的呈现，语义色保留（详见 Python 版注释）。
/// 颜色值与前端 styles/tokens/color.css 的 dark token 一致——iframe 是不透明源，
/// 读不到父页 CSS 变量，只能镜像字面量。
const THEME_STYLE: &str = concat!(
    "<style>",
    "html[data-mydict-theme='dark']{color-scheme:dark;background-color:transparent}",
    "html[data-mydict-theme='dark'] body",
    "{background-color:transparent;background-image:none;color:#eaf1ee}",
    "html[data-mydict-theme='dark'] [color='#000' i],",
    "html[data-mydict-theme='dark'] [color='#000000' i],",
    "html[data-mydict-theme='dark'] [color='black' i]{color:#eaf1ee}",
    "html[data-mydict-theme='dark'] [style*='color:#000' i],",
    "html[data-mydict-theme='dark'] [style*='color: #000' i],",
    "html[data-mydict-theme='dark'] [style*='color:black' i],",
    "html[data-mydict-theme='dark'] [style*='color: black' i],",
    "html[data-mydict-theme='dark'] [style*='color:rgb(0,0,0)' i],",
    "html[data-mydict-theme='dark'] [style*='color: rgb(0, 0, 0)' i]{color:#eaf1ee !important}",
    "html[data-mydict-theme='dark'] [bgcolor='#fff' i],",
    "html[data-mydict-theme='dark'] [bgcolor='#ffffff' i],",
    "html[data-mydict-theme='dark'] [bgcolor='white' i],",
    "html[data-mydict-theme='dark'] [style*='background:#fff' i],",
    "html[data-mydict-theme='dark'] [style*='background: #fff' i],",
    "html[data-mydict-theme='dark'] [style*='background-color:#fff' i],",
    "html[data-mydict-theme='dark'] [style*='background-color: #fff' i],",
    "html[data-mydict-theme='dark'] [style*='background:white' i],",
    "html[data-mydict-theme='dark'] [style*='background: white' i]",
    "{background-color:transparent !important}",
    "</style>"
);

/// 词典常用负外边距做「出血」（千篇 ul 左右各 6px），会引出两根滚动条；
/// `clip` 只裁掉、不产生滚动容器（hidden 会让 html 变成滚动容器）
const NO_HSCROLL_STYLE: &str = "<style>html{overflow-x:clip}</style>";

/// 选中文字弹【查词】按钮的样式（fixed 定位不参与 scrollHeight，不干扰高度上报）
const LOOKUP_STYLE: &str = concat!(
    "<style>",
    ".mydict-lookup-bar{position:fixed;z-index:2147483647;display:flex;gap:6px;",
    "font-family:system-ui,-apple-system,'PingFang SC','Microsoft YaHei',sans-serif}",
    ".mydict-lookup{padding:4px 12px;border-radius:6px;",
    "font-size:13px;line-height:1.7;cursor:pointer;user-select:none;-webkit-user-select:none;",
    "white-space:nowrap;background:#fff;color:#0f2b22;border:1px solid #d8e2de;",
    "box-shadow:0 4px 14px rgba(0,0,0,.16)}",
    ".mydict-lookup-tts{color:#0b62d6;border-color:#c5d8f2}",
    "[data-mydict-theme='dark'] .mydict-lookup{background:#1c2a25;color:#eaf1ee;",
    "border-color:#33443d;box-shadow:0 4px 14px rgba(0,0,0,.5)}",
    "[data-mydict-theme='dark'] .mydict-lookup-tts{color:#8ab4f8;border-color:#2d4470}",
    "</style>"
);

/// 同一词典同词头多条排成一列的小标题样式（content-visibility 跳过视口外分节绘制）
const MULTI_ENTRY_STYLE: &str = concat!(
    "<style>",
    ".mydict-entry+.mydict-entry{margin-top:14px;padding-top:12px;",
    "border-top:1px solid #dfe7e4}",
    ".mydict-entry-head{margin:0 0 8px;font-size:13px;line-height:1.6;color:#5b6b66;",
    "font-family:system-ui,-apple-system,'PingFang SC','Microsoft YaHei',sans-serif}",
    ".mydict-entry-index{display:inline-block;min-width:1.6em;margin-right:6px;",
    "font-variant-numeric:tabular-nums;color:#8b9a95}",
    ".mydict-entry-word{font-weight:600;font-size:15px;color:#12211d}",
    ".mydict-entry-phonetic{margin-left:6px;color:#8b9a95}",
    "[data-mydict-theme='dark'] .mydict-entry+.mydict-entry{border-top-color:#33443d}",
    "[data-mydict-theme='dark'] .mydict-entry-head{color:#9fb0aa}",
    "[data-mydict-theme='dark'] .mydict-entry-index{color:#7d8f89}",
    "[data-mydict-theme='dark'] .mydict-entry-word{color:#eaf1ee}",
    "[data-mydict-theme='dark'] .mydict-entry-phonetic{color:#7d8f89}",
    ".mydict-entry{content-visibility:auto;contain-intrinsic-size:auto 500px}",
    "</style>"
);

fn theme_init_script(theme: Option<&str>) -> String {
    // 只认 light/dark，其它输入当作「没指定」；写死主题消掉首屏闪变
    match theme {
        Some("light") | Some("dark") => {
            let t = theme.unwrap_or_default();
            format!(
                "<script>document.documentElement.setAttribute('data-mydict-theme','{t}')</script>"
            )
        }
        _ => String::new(),
    }
}

/// 要插进文档最前面的内容：编码/Referer 策略 + 主题初值 + 样式 + 引导脚本。
/// 引导脚本必须排在词典自带脚本之前；css link 排在引导脚本之前、词典 js 排在之后。
fn head_snippet(
    dictionary_id: i32,
    theme: Option<&str>,
    allow_lookup: bool,
    multi_entry: bool,
    extra_head_assets: &[(String, String)],
) -> String {
    let bootstrap = BOOTSTRAP_SOURCE
        .replace("__MYDICT_DICT_ID__", &dictionary_id.to_string())
        .replace("__MYDICT_LOOKUP__", if allow_lookup { "true" } else { "false" });
    let lookup_style = if allow_lookup { LOOKUP_STYLE } else { "" };
    let multi_style = if multi_entry { MULTI_ENTRY_STYLE } else { "" };
    let mut css_links = String::new();
    let mut js_scripts = String::new();
    for (name, url) in extra_head_assets {
        if name.to_lowercase().ends_with(".css") {
            css_links.push_str(&format!(r#"<link rel="stylesheet" href="{url}">"#));
        } else if name.to_lowercase().ends_with(".js") {
            js_scripts.push_str(&format!(r#"<script src="{url}"></script>"#));
        }
    }
    [
        r#"<meta charset="utf-8">"#.to_string(),
        // 不把本站地址带给出站请求
        r#"<meta name="referrer" content="no-referrer">"#.to_string(),
        format!(r#"<meta http-equiv="Content-Security-Policy" content="{SANDBOX_CSP}">"#),
        theme_init_script(theme),
        THEME_STYLE.to_string(),
        NO_HSCROLL_STYLE.to_string(),
        multi_style.to_string(),
        lookup_style.to_string(),
        css_links,
        format!("<script>{bootstrap}</script>"),
        js_scripts,
    ]
    .concat()
}

/// 丢掉词条 HTML 里已经自己引用的同名资源，避免同一份文件加载两次
fn filter_referenced_assets(
    extra_head_assets: &[(String, String)],
    definitions: &[&str],
) -> Vec<(String, String)> {
    let haystack: String = definitions
        .iter()
        .map(|d| d.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    extra_head_assets
        .iter()
        .filter(|(name, _)| !haystack.contains(&name.to_lowercase()))
        .cloned()
        .collect()
}

pub struct RenderEntry<'a> {
    pub word: &'a str,
    pub definition: &'a str,
    pub phonetic: Option<&'a str>,
}

/// 把一组词条渲染成一个完整 HTML 文档。单条时的输出与旧版逐字节一致。
/// 释义本身是完整文档（带 doctype/html）时不套壳，把 head 片段插进它自己的
/// `<head>`（或 `<html>`）之后；多条时任一条是完整文档则只渲染第一条。
#[allow(clippy::too_many_arguments)]
pub fn render_entries_document(
    entries: &[RenderEntry<'_>],
    dictionary_id: i32,
    theme: Option<&str>,
    allow_lookup: bool,
    extra_head_assets: &[(String, String)],
) -> String {
    if entries.is_empty() {
        return render_entries_document(
            &[RenderEntry { word: "", definition: "", phonetic: None }],
            dictionary_id,
            theme,
            allow_lookup,
            &[],
        );
    }

    let single = entries.len() == 1;
    if !single && entries.iter().any(|e| DOCTYPE_OR_HTML_RE.is_match(e.definition)) {
        tracing::warn!(
            dictionary_id,
            "同名词条里有完整文档型释义，无法合并，只渲染第一条"
        );
        let first = &entries[0];
        return render_entries_document(
            &[first.clone_ref()],
            dictionary_id,
            theme,
            allow_lookup,
            extra_head_assets,
        );
    }

    let definitions: Vec<&str> = entries.iter().map(|e| e.definition).collect();
    let assets = filter_referenced_assets(extra_head_assets, &definitions);
    let head = head_snippet(dictionary_id, theme, allow_lookup, !single, &assets);

    let body = if single {
        let definition = entries[0].definition;
        if DOCTYPE_OR_HTML_RE.is_match(definition) {
            let m = HEAD_OPEN_RE
                .find(definition)
                .or_else(|| HTML_OPEN_RE.find(definition));
            match m {
                Some(m) => {
                    let at = m.end();
                    return format!("{}{}{}", &definition[..at], head, &definition[at..]);
                }
                // 既没有 <head> 也没有 <html>，只能在最前面插
                None => return format!("{head}{definition}"),
            }
        }
        definition.to_string()
    } else {
        let total = entries.len();
        let mut blocks = String::new();
        for (index, entry) in entries.iter().enumerate() {
            let index = index + 1;
            let mut label = format!(
                r#"<span class="mydict-entry-index">{index}/{total}</span><span class="mydict-entry-word">{}</span>"#,
                html_escape(entry.word)
            );
            if let Some(phonetic) = entry.phonetic {
                label.push_str(&format!(
                    r#"<span class="mydict-entry-phonetic">[{}]</span>"#,
                    html_escape(phonetic)
                ));
            }
            blocks.push_str(&format!(
                r#"<section class="mydict-entry"><div class="mydict-entry-head">{label}</div>{}</section>"#,
                entry.definition
            ));
        }
        blocks
    };

    format!(
        "<!DOCTYPE html><html lang=\"zh\"><head>{head}</head><body>{body}</body></html>"
    )
}

/// Python html.escape(s, quote=True) 等价
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&#x27;")
}

impl<'a> RenderEntry<'a> {
    fn clone_ref(&self) -> RenderEntry<'a> {
        RenderEntry {
            word: self.word,
            definition: self.definition,
            phonetic: self.phonetic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets() -> Vec<(String, String)> {
        Vec::new()
    }

    #[test]
    fn single_entry_document_shape() {
        let doc = render_entries_document(
            &[RenderEntry { word: "apple", definition: "<b>apple</b>", phonetic: Some("ˈæpl") }],
            7,
            None,
            false,
            &assets(),
        );
        assert!(doc.starts_with("<!DOCTYPE html><html lang=\"zh\"><head>"));
        assert!(doc.ends_with("</head><body><b>apple</b></body></html>"));
        assert!(doc.contains("sandbox allow-scripts"));
        // 引导脚本已替换占位符
        assert!(doc.contains("__MYDICT_DICT_ID__") == false);
        assert!(doc.contains("document.documentElement"));
    }

    #[test]
    fn theme_written_into_document() {
        let doc = render_entries_document(
            &[RenderEntry { word: "w", definition: "d", phonetic: None }],
            1,
            Some("dark"),
            false,
            &assets(),
        );
        assert!(doc.contains("data-mydict-theme','dark'"));
        // 非法主题视同未传
        let doc2 = render_entries_document(
            &[RenderEntry { word: "w", definition: "d", phonetic: None }],
            1,
            Some("neon"),
            false,
            &assets(),
        );
        assert!(!doc2.contains("neon"));
    }

    #[test]
    fn full_document_definition_not_wrapped() {
        let definition = "<!DOCTYPE html><html><head><title>x</title></head><body>hi</body></html>";
        let doc = render_entries_document(
            &[RenderEntry { word: "w", definition, phonetic: None }],
            3,
            None,
            false,
            &assets(),
        );
        // 不产生嵌套 html：head 片段插进它自己的 <head> 之后（紧跟 <head>，<title> 在其后）
        assert!(doc.starts_with("<!DOCTYPE html><html><head><meta charset=\"utf-8\">"));
        assert!(doc.contains("<title>x</title>"));
        assert!(!doc.contains("<!DOCTYPE html><!DOCTYPE"));
        assert!(doc.contains("sandbox allow-scripts"));
    }

    #[test]
    fn multi_entry_sections() {
        let doc = render_entries_document(
            &[
                RenderEntry { word: "bank", definition: "<p>银行</p>", phonetic: Some("bæŋk") },
                RenderEntry { word: "bank", definition: "<p>河岸</p>", phonetic: None },
            ],
            1,
            None,
            false,
            &assets(),
        );
        assert!(doc.contains("mydict-entry-index\">1/2</span>"));
        assert!(doc.contains("mydict-entry-index\">2/2</span>"));
        assert!(doc.contains("mydict-entry-phonetic\">[bæŋk]</span>"));
        assert!(doc.contains("content-visibility:auto"));
    }

    #[test]
    fn same_name_assets_injected_and_filtered() {
        let with_assets = vec![
            ("dict.css".to_string(), "/dict-res/1/res/dict.css".to_string()),
            ("dict.js".to_string(), "/dict-res/1/res/dict.js".to_string()),
        ];
        let doc = render_entries_document(
            &[RenderEntry { word: "w", definition: "<p>hi</p>", phonetic: None }],
            1,
            None,
            false,
            &with_assets,
        );
        // css link 在引导脚本前，js 在其后
        let css_at = doc.find(r#"<link rel="stylesheet" href="/dict-res/1/res/dict.css">"#).unwrap();
        let js_at = doc.find(r#"<script src="/dict-res/1/res/dict.js"></script>"#).unwrap();
        let bootstrap_at = doc.find("<script>/*").or_else(|| doc.find("document.documentElement")).unwrap();
        assert!(css_at < bootstrap_at && bootstrap_at < js_at);
        // 词条已引用的同名 css 被过滤
        let doc2 = render_entries_document(
            &[RenderEntry { word: "w", definition: "<p><link href=\"/dict-res/1/res/dict.css\">hi</p>", phonetic: None }],
            1,
            None,
            false,
            &with_assets,
        );
        assert_eq!(doc2.matches("dict.css\"></link>").count(), 0);
        assert!(doc2.matches(r#"href="/dict-res/1/res/dict.css""#).count() <= 2);
    }

    #[test]
    fn allow_lookup_injects_menu_style_and_flag() {
        let doc = render_entries_document(
            &[RenderEntry { word: "w", definition: "d", phonetic: None }],
            1,
            None,
            true,
            &assets(),
        );
        assert!(doc.contains(".mydict-lookup{"));
        let no = render_entries_document(
            &[RenderEntry { word: "w", definition: "d", phonetic: None }],
            1,
            None,
            false,
            &assets(),
        );
        assert!(!no.contains(".mydict-lookup{"));
    }

    #[test]
    fn bootstrap_contains_no_document_literals() {
        // Python 测试锁定的断言：引导脚本不能包含 <html>/<body>/<!doctype 字面量
        // （它会以 <script> 内联进 head，出现这些字面量说明拿错了文件）
        assert!(!BOOTSTRAP_SOURCE.contains("<html>"));
        assert!(!BOOTSTRAP_SOURCE.contains("<body>"));
        assert!(!BOOTSTRAP_SOURCE.contains("<!doctype"));
        assert!(!BOOTSTRAP_SOURCE.contains("<!DOCTYPE"));
    }

    #[test]
    fn word_escaped_in_multi_entry_header() {
        // 单条路径 body 只是释义本身（词头不渲染）；转义只在多条小标题里发生
        let doc = render_entries_document(
            &[
                RenderEntry { word: "<script>alert(1)</script>", definition: "d1", phonetic: None },
                RenderEntry { word: "b", definition: "d2", phonetic: None },
            ],
            1,
            None,
            false,
            &assets(),
        );
        assert!(!doc.contains("<script>alert"));
        assert!(doc.contains("&lt;script&gt;"));
    }
}

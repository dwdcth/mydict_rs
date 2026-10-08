//! 在线词典（维基百科/维基词典/百度百科 + 外链）—— 移植自 `app/services/online_dict_service.py`。

use moka::sync::Cache as MokaCache;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use crate::core::config::Settings;
use crate::core::errors::AppError;
use crate::services::settings_service;
use crate::AppState;

const HTTP_TIMEOUT: Duration = Duration::from_secs(6);
const BAIKE_NOT_FOUND: &str = "百度百科尚未收录词条";

/// 管理后台「在线词典」开关的合法 id 顺序（section 源 + 外链）
pub const SOURCE_IDS: &[&str] = &[
    "wikipedia",
    "wiktionary",
    "baike",
    "google",
    "urban",
    "merriam",
    "goodreads",
];

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static SPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// 在线结果缓存（10 分钟 / 32MB 按体积计重）：这些站点对高频抓取不友好；
/// 词条 JSON 大小悬殊（维基长文几百 KB），按条数限会失守
static CACHE: LazyLock<MokaCache<String, Arc<Value>>> = LazyLock::new(|| {
    MokaCache::builder()
        .max_capacity(32 * 1024 * 1024)
        .weigher(|_k, v: &Arc<Value>| {
            crate::core::query_cache::json_weight(v).min(u32::MAX as u64) as u32
        })
        .time_to_live(Duration::from_secs(600))
        .build()
});

/// 出站代理（DB 覆盖 env；变化时清缓存）——热同步
static ACTIVE_PROXY: LazyLock<RwLock<String>> = LazyLock::new(|| RwLock::new(String::new()));

struct OnlineState {
    client: reqwest::Client,
    /// 走当前代理的维基专用客户端（代理串变化时重建；百度百科恒直连）
    proxy_client: RwLock<(String, reqwest::Client)>,
}

static STATE: LazyLock<OnlineState> = LazyLock::new(|| OnlineState {
    // 重定向不跟随（对齐 Python httpx 默认 follow_redirects=False：302 视为源不可用）
    client: reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client"),
    proxy_client: RwLock::new((String::new(), reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client"))),
});

/// 维基两源出站客户端：配置了代理则走代理（对齐 Python 每请求带 proxy=…）
fn wiki_client() -> reqwest::Client {
    let proxy = active_proxy();
    let guard = STATE
        .proxy_client
        .read()
        .unwrap_or_else(|e| e.into_inner());
    if guard.0.is_empty() || guard.0 != proxy {
        drop(guard);
        let mut guard = STATE
            .proxy_client
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if guard.0 != proxy {
            let builder = reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none());
            let built = if proxy.is_empty() {
                builder.build()
            } else {
                match reqwest::Proxy::all(&proxy) {
                    Ok(p) => builder.proxy(p).build(),
                    Err(_) => builder.build(),
                }
            };
            if let Ok(client) = built {
                *guard = (proxy, client);
            }
        }
        return guard.1.clone();
    }
    guard.1.clone()
}

fn active_proxy() -> String {
    ACTIVE_PROXY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn configure_proxy(proxy: Option<&str>) {
    let new_value = proxy.unwrap_or("").to_string();
    let mut guard = ACTIVE_PROXY.write().unwrap_or_else(|e| e.into_inner());
    if *guard != new_value {
        *guard = new_value;
        CACHE.invalidate_all();
    }
}

/// 维基词典释义 HTML 片段 → 纯文本（服务端剥掉，第三方 HTML 不进页面）
fn strip_html(html: &str) -> String {
    let text = TAG_RE.replace_all(html, "");
    let decoded = html_escape::decode_html_entities(&text).to_string();
    SPACE_RE.replace_all(&decoded, " ").trim().to_string()
}

fn meta_property(html: &str, property: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"<meta[^>]+property="{property}"[^>]+content="([^"]*)""#
    ))
    .ok()?;
    let re2 = Regex::new(&format!(
        r#"<meta[^>]+content="([^"]*)"[^>]+property="{property}""#
    ))
    .ok()?;
    let m = re
        .captures(html)
        .or_else(|| re2.captures(html))?;
    Some(
        html_escape::decode_html_entities(&m[1])
            .to_string(),
    )
}

fn encode_word(word: &str) -> String {
    // urllib.parse.quote 的默认安全集（字母数字/_.-~/ 保持原样）
    let mut out = String::new();
    for byte in word.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn external_links(word: &str, enabled: &std::collections::HashSet<String>) -> Vec<Value> {
    let encoded = encode_word(word);
    let all = [
        ("google", "Google", format!("https://www.google.com/search?q=define:{encoded}&hl=en")),
        ("urban", "Urban Dictionary", format!("https://www.urbandictionary.com/define.php?term={encoded}")),
        ("merriam", "Merriam-Webster", format!("https://www.merriam-webster.com/dictionary/{encoded}")),
        ("goodreads", "Goodreads", format!("https://www.goodreads.com/search?q={encoded}")),
    ];
    all.iter()
        .filter(|(id, _, _)| enabled.contains(&id.to_string()))
        .map(|(id, name, url)| json!({"id": id, "name": name, "url": url}))
        .collect()
}

async fn fetch_wikipedia(word: &str, lang: &str) -> Result<Option<Value>, String> {
    let url = format!(
        "https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}",
        encode_word(word)
    );
    let resp = wiki_client()
        .get(&url)
        .header("User-Agent", "mydict-dictionary/1.0 (self-hosted dictionary server)")
        .header("Api-User-Agent", "mydict-dictionary/1.0 (self-hosted dictionary server)")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        404 => return Ok(None),
        200 => {}
        code => return Err(format!("HTTP {code}")),
    }
    let data: Value = resp.json().await.map_err(|e| e.to_string())?;
    let extract = data["extract"].as_str().unwrap_or("").trim().to_string();
    if extract.is_empty() {
        return Ok(None);
    }
    // titles.display 是 HTML（<span lang=…>苹果</span>），剥成纯文本
    let display = strip_html(data["titles"]["display"].as_str().unwrap_or(""));
    Ok(Some(json!({
        "id": "wikipedia",
        "name": "Wikipedia",
        "title": if display.is_empty() {
            data["title"].as_str().unwrap_or(word).to_string()
        } else {
            display
        },
        "subtitle": data["description"].as_str().unwrap_or(""),
        "text": extract,
        "url": data["content_urls"]["desktop"]["page"].clone(),
    })))
}

/// 维基词典释义按语言分组；查询语言优先，退 en，再退第一个有内容的
fn pick_wiktionary_lang<'a>(data: &'a Value, lang: &str) -> Option<&'a Vec<Value>> {
    let obj = data.as_object()?;
    let keys: Vec<&String> = if lang != "en" {
        let mut k: Vec<&String> = obj.keys().filter(|key| key.as_str() == lang).collect();
        k.extend(obj.keys().filter(|key| key.as_str() == "en"));
        k
    } else {
        obj.keys().filter(|key| key.as_str() == "en").collect()
    };
    for key in keys {
        if let Some(arr) = obj[key].as_array() {
            if !arr.is_empty() {
                return Some(arr);
            }
        }
    }
    obj.values().find_map(|v| {
        v.as_array().filter(|arr| !arr.is_empty()).map(|arr| arr as &Vec<Value>)
    })
}

async fn fetch_wiktionary(word: &str, lang: &str) -> Result<Option<Value>, String> {
    let url = format!(
        "https://en.wiktionary.org/api/rest_v1/page/definition/{}",
        encode_word(word)
    );
    let resp = wiki_client()
        .get(&url)
        .header("User-Agent", "mydict-dictionary/1.0 (self-hosted dictionary server)")
        .header("Api-User-Agent", "mydict-dictionary/1.0 (self-hosted dictionary server)")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        404 => return Ok(None),
        200 => {}
        code => return Err(format!("HTTP {code}")),
    }
    let data: Value = resp.json().await.map_err(|e| e.to_string())?;
    let Some(results) = pick_wiktionary_lang(&data, lang) else {
        return Ok(None);
    };
    let mut entries = Vec::new();
    for item in results.iter().take(4) {
        let mut senses = Vec::new();
        if let Some(definitions) = item["definitions"].as_array() {
            for definition in definitions.iter().take(8) {
                let text = strip_html(definition["definition"].as_str().unwrap_or(""));
                if text.is_empty() {
                    continue;
                }
                let mut examples = Vec::new();
                if let Some(exs) = definition["examples"].as_array() {
                    for ex in exs.iter().take(3) {
                        let stripped = strip_html(ex.as_str().unwrap_or(""));
                        if !stripped.is_empty() {
                            examples.push(stripped);
                        }
                    }
                }
                senses.push(json!({"text": text, "examples": examples}));
            }
        }
        if !senses.is_empty() {
            entries.push(json!({
                "pos": item["partOfSpeech"].as_str().unwrap_or(""),
                "language": item["language"].as_str().unwrap_or(""),
                "senses": senses,
            }));
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({
        "id": "wiktionary",
        "name": "Wiktionary",
        "entries": entries,
    })))
}

async fn fetch_baike(word: &str) -> Result<Option<Value>, String> {
    // 桌面 item 页对非浏览器请求一律弹「百度安全验证」，唯一放行 /search/word + 手机 UA
    let url = format!(
        "https://baike.baidu.com/search/word?pic=1&enc=utf-8&word={}",
        encode_word(word)
    );
    let resp = STATE
        .client
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Linux; U; Android 16;) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Mobile Safari/537.36",
        )
        .header("Accept-Language", "zh-CN,zh;q=0.8,zh-TW;q=0.6")
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        404 => return Ok(None),
        200 => {}
        code => return Err(format!("HTTP {code}")),
    }
    let final_url = resp.url().to_string();
    let html = resp.text().await.map_err(|e| e.to_string())?;
    if html.contains("<title>验证") {
        return Err("百度安全验证".to_string());
    }
    if html.contains(BAIKE_NOT_FOUND) {
        return Ok(None);
    }
    let Some(description) = meta_property(&html, "og:description") else {
        return Ok(None);
    };
    let title = meta_property(&html, "og:title").unwrap_or_else(|| word.to_string());
    // doc.title 形如「苹果（蔷薇科苹果属植物）_百度百科」——括号限定语作 subtitle
    let title_re = Regex::new(r"<title>([^<]*)</title>").unwrap();
    let descriptor = title_re
        .captures(&html)
        .and_then(|caps| {
            let unescaped = html_escape::decode_html_entities(&caps[1]).to_string();
            Regex::new(r"[（(]([^）)]*)[）)]")
                .unwrap()
                .captures(&unescaped)
                .map(|c| c[1].to_string())
        })
        .unwrap_or_default();
    Ok(Some(json!({
        "id": "baike",
        "name": "百度百科",
        "title": title,
        "subtitle": descriptor,
        "text": description,
        "url": final_url,
    })))
}

/// 查询在线词典；单个源失败不影响其它源（失败结果不入缓存）
pub async fn lookup_online(
    word: &str,
    lang: &str,
    enabled_sources: Option<&std::collections::HashSet<String>>,
) -> Value {
    let all_ids: std::collections::HashSet<String> = SOURCE_IDS.iter().map(|s| s.to_string()).collect();
    let enabled_owned: std::collections::HashSet<String>;
    let enabled: &std::collections::HashSet<String> = match enabled_sources {
        Some(set) if !set.is_empty() => {
            enabled_owned = set.clone();
            &enabled_owned
        }
        _ => &all_ids,
    };
    let mut sorted_enabled: Vec<String> = enabled.iter().map(|s| s.to_string()).collect();
    sorted_enabled.sort_unstable();
    let cache_key = format!("{word}\u{1}{lang}\u{1}{}", sorted_enabled.join(","));
    if let Some(cached) = CACHE.get(&cache_key) {
        return (*cached).clone();
    }

    // 三个源并发（各源是不同的 future 类型，用元组 + Option 组合）
    let has = |id: &str| enabled.contains(id);
    // 三个源并发；未启用的源用「立即返回 None」的占位 future（同一泛型 join3）
    async fn noop() -> Result<Option<Value>, String> {
        Ok(None)
    }
    let enable_wiki = has("wikipedia");
    let enable_wikt = has("wiktionary");
    let enable_baike = has("baike");
    let (wiki_res, wikt_res, baike_res) = futures::future::join3(
        async { if enable_wiki { fetch_wikipedia(word, lang).await } else { noop().await } },
        async { if enable_wikt { fetch_wiktionary(word, lang).await } else { noop().await } },
        async { if enable_baike { fetch_baike(word).await } else { noop().await } },
    )
    .await;

    let mut sections = Vec::new();
    let mut failed = false;
    for (id, result) in [("wikipedia", wiki_res), ("wiktionary", wikt_res), ("baike", baike_res)] {
        match result {
            Ok(Some(section)) => sections.push(section),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(source = id, word, error = %err, "在线词典源查询失败");
                failed = true;
            }
        }
    }
    let result = json!({
        "word": word,
        "lang": lang,
        "sections": sections,
        "links": external_links(word, enabled),
    });
    if !failed {
        CACHE.insert(cache_key, Arc::new(result.clone()));
    }
    result
}

/// 按管理后台设置查询：总开关、出站代理（DB 覆盖 env 热同步）、源白名单
pub async fn lookup_with_settings(
    state: &AppState,
    defaults: &Settings,
    word: &str,
    lang: &str,
) -> Result<Value, AppError> {
    let enabled_flag = settings_service::get_bool_setting(&state.db, "online_dict_enabled", false).await?;
    if !enabled_flag {
        return Err(AppError::forbidden("在线词典功能未启用，请联系管理员在系统设置中开启"));
    }
    let proxy = settings_service::get_setting(
        &state.db,
        "online_dict_proxy",
        Some(&defaults.online_dict_proxy),
    )
    .await?
    .unwrap_or_default()
    .trim()
    .to_string();
    if proxy != active_proxy() {
        configure_proxy((!proxy.is_empty()).then_some(proxy.as_str()));
    }
    let word: String = word.trim().chars().take(100).collect();
    if word.is_empty() {
        return Ok(json!({"word": "", "lang": lang, "sections": [], "links": []}));
    }
    let raw = settings_service::get_setting(&state.db, "online_dict_sources", Some(""))
        .await?
        .unwrap_or_default()
        .trim()
        .to_string();
    let enabled: std::collections::HashSet<String> = raw
        .split(',')
        .filter(|sid| SOURCE_IDS.contains(sid))
        .map(String::from)
        .collect();
    let enabled = (!enabled.is_empty()).then_some(enabled);
    Ok(lookup_online(&word, lang, enabled.as_ref()).await)
}

#[allow(dead_code)]
fn unused(_: &Map<String, Value>) {}

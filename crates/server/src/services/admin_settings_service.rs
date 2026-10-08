//! 管理后台系统设置聚合 —— 移植自 `app/services/admin_settings_service.py`。
//!
//! 新增（Rust 版）：`query_log_retention_days`（query_logs 保留期，空 = 永久）。
//! 该键是响应超集（前端忽略未知字段），不影响契约。

use sea_orm::DatabaseConnection;
use crate::AppState;
use serde_json::{json, Map, Value};

use crate::core::config::Settings;
use crate::core::errors::AppError;
use crate::services::{audit_service, settings_service};

/// 在线词典源白名单（固定顺序）。M5 online_dict_service 使用同一常量。
pub const SOURCE_IDS: &[&str] = &[
    "wikipedia",
    "wiktionary",
    "baike",
    "google",
    "urban",
    "merriam",
    "goodreads",
];

const BOOL_KEYS: &[&str] = &[
    "open_access",
    "allow_registration",
    "online_dict_enabled",
    "random_browse_enabled",
    "tts_enabled",
];
const INT_KEYS: &[&str] = &[
    "token_default_daily_limit",
    "anonymous_ip_rate_limit_per_min",
    "user_ip_rate_limit_per_min",
];
const OPTIONAL_INT_KEYS: &[&str] = &["vocab_max_items_per_owner", "query_log_retention_days"];
const STR_KEYS: &[&str] = &[
    "site_name",
    "search_hint_text",
    "online_dict_sources",
    "tts_engine",
    "tts_voice_zh",
    "tts_voice_en",
];

const SEARCH_HINT_DEFAULT: &str = "小搜一下, 大进一步";

/// 把用户输入的 CSV 归一化成固定顺序的白名单 CSV；非法 id 忽略，空值 = 全部启用。
fn normalize_online_sources(raw: Option<&str>) -> String {
    let Some(raw) = raw else { return String::new() };
    if raw.is_empty() {
        return String::new();
    }
    let picked: Vec<&str> = raw.split(',').map(|s| s.trim()).collect();
    SOURCE_IDS
        .iter()
        .filter(|sid| picked.contains(sid))
        .copied()
        .collect::<Vec<_>>()
        .join(",")
}

/// 出站代理只接受 http(s)://host[:port]：reqwest 未开 socks 时其它写法每次请求都会失败
fn normalize_proxy(raw: Option<&str>) -> Result<String, AppError> {
    let value = raw.unwrap_or_default().trim().to_string();
    if value.is_empty() {
        return Ok(String::new());
    }
    let valid = {
        let rest = value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"));
        match rest {
            Some(rest) => {
                let host = rest.split(['/', '?']).next().unwrap_or("");
                // host[:port]
                let host_part = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
                !host_part.is_empty() && !host_part.contains(['@', ' '])
            }
            None => false,
        }
    };
    if !valid {
        return Err(AppError::validation(
            "出站代理须为 http:// 或 https:// 开头的地址，如 http://127.0.0.1:7890",
        ));
    }
    Ok(value)
}

/// 与部署环境变量 ONLINE_DICT_PROXY 相同（含都为空）时删掉覆盖项、回落到 env；
/// 否则存为覆盖值——空串表示在 env 配了代理的部署上改为直连。
async fn save_online_dict_proxy(
    db: &DatabaseConnection,
    raw: Option<&str>,
    defaults: &Settings,
) -> Result<String, AppError> {
    let value = normalize_proxy(raw)?;
    if value == defaults.online_dict_proxy.trim() {
        settings_service::delete_setting(db, "online_dict_proxy").await?;
    } else {
        settings_service::set_setting(db, "online_dict_proxy", &value).await?;
    }
    Ok(value)
}

pub async fn get_all_settings(db: &DatabaseConnection, defaults: &Settings) -> Result<Value, sea_orm::DbErr> {
    Ok(json!({
        "open_access": settings_service::get_bool_setting(db, "open_access", defaults.open_access_default).await?,
        "allow_registration": settings_service::get_bool_setting(db, "allow_registration", defaults.allow_registration_default).await?,
        // 在线词典总开关：默认禁用（出站抓取第三方站点，是否开放由部署者决定）
        "online_dict_enabled": settings_service::get_bool_setting(db, "online_dict_enabled", false).await?,
        // 随机浏览开关：默认禁用。开启后会预热词典主键区间缓存，当前台【随机】标签的门控
        "random_browse_enabled": settings_service::get_bool_setting(db, "random_browse_enabled", false).await?,
        "token_default_daily_limit": settings_service::get_int_setting(db, "token_default_daily_limit", defaults.token_default_daily_limit).await?,
        "anonymous_ip_rate_limit_per_min": settings_service::get_int_setting(db, "anonymous_ip_rate_limit_per_min", defaults.anonymous_ip_rate_limit_per_min).await?,
        "user_ip_rate_limit_per_min": settings_service::get_int_setting(db, "user_ip_rate_limit_per_min", defaults.user_ip_rate_limit_per_min).await?,
        "vocab_max_items_per_owner": get_optional_int(db, "vocab_max_items_per_owner").await?,
        "query_log_retention_days": get_optional_int(db, "query_log_retention_days").await?,
        "site_name": settings_service::get_setting(db, "site_name", Some("MyDict")).await?,
        "search_hint_text": settings_service::get_setting(db, "search_hint_text", Some(SEARCH_HINT_DEFAULT)).await?,
        "online_dict_proxy": settings_service::get_setting(db, "online_dict_proxy", Some(&defaults.online_dict_proxy)).await?,
        "online_dict_sources": normalize_online_sources(settings_service::get_setting(db, "online_dict_sources", Some("")).await?.as_deref()),
        // TTS（kokoro-micro 内嵌引擎）：默认关。开启后词条头部无词典语音时显示「喇叭+T」
        "tts_enabled": settings_service::get_bool_setting(db, "tts_enabled", false).await?,
        "tts_voice_zh": settings_service::get_setting(db, "tts_voice_zh", Some("zf_xiaoni")).await?,
        "tts_voice_en": settings_service::get_setting(db, "tts_voice_en", Some("af_heart")).await?,
        // edge（微软在线，默认）| kokoro（本地离线）
        "tts_engine": settings_service::get_setting(db, "tts_engine", Some("edge")).await?,
        // TTS 注音提取规则（数组形态给前端；库里的 JSON 字符串坏掉时降级为空）
        "tts_pinyin_rules": get_pinyin_rules_as_value(db).await,
    }))
}

/// tts_pinyin_rules 库值（JSON 字符串）→ 数组 Value；解析失败给空数组
async fn get_pinyin_rules_as_value(db: &DatabaseConnection) -> Value {
    let raw = settings_service::get_setting(db, "tts_pinyin_rules", Some(""))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let parsed = crate::services::tts::parse_pinyin_rules(&serde_json::Value::String(raw));
    match parsed {
        Ok(rules) => serde_json::to_value(&rules).unwrap_or(serde_json::json!([])),
        Err(_) => serde_json::json!([]),
    }
}

pub async fn get_public_settings(db: &DatabaseConnection, defaults: &Settings, initialized: bool) -> Result<Value, sea_orm::DbErr> {
    Ok(json!({
        "open_access": settings_service::get_bool_setting(db, "open_access", defaults.open_access_default).await?,
        "allow_registration": settings_service::get_bool_setting(db, "allow_registration", defaults.allow_registration_default).await?,
        // 前台要靠它决定是否渲染【在线】标签
        "online_dict_enabled": settings_service::get_bool_setting(db, "online_dict_enabled", false).await?,
        "random_browse_enabled": settings_service::get_bool_setting(db, "random_browse_enabled", false).await?,
        "site_name": settings_service::get_setting(db, "site_name", Some("MyDict")).await?,
        "initialized": initialized,
        "search_hint_text": settings_service::get_setting(db, "search_hint_text", Some(SEARCH_HINT_DEFAULT)).await?,
    }))
}

async fn get_optional_int(db: &DatabaseConnection, key: &str) -> Result<Option<i64>, sea_orm::DbErr> {
    match settings_service::get_setting(db, key, None).await? {
        Some(raw) if !raw.trim().is_empty() => Ok(raw.trim().parse().ok()),
        _ => Ok(None),
    }
}

/// updates 只处理 fields_present 里实际出现过的字段，区分「未传」与「显式传 null」。
pub async fn update_settings(
    state: &std::sync::Arc<AppState>,
    updates: &Map<String, Value>,
    fields_present: &[String],
    defaults: &Settings,
    admin_id: i32,
) -> Result<Value, AppError> {
    // 先校验代理（非法时一个字段都不写，对齐 Python 语义）
    if fields_present.iter().any(|k| k == "online_dict_proxy") {
        normalize_proxy(updates.get("online_dict_proxy").and_then(|v| v.as_str()))?;
    }
    // 随机浏览从关到开的那一刻就开始预热区间缓存
    let mut warm_random = false;
    if fields_present.iter().any(|k| k == "random_browse_enabled")
        && updates
            .get("random_browse_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        warm_random =
            !settings_service::get_bool_setting(&state.db, "random_browse_enabled", false).await?;
    }

    let mut changed = Map::new();
    for key in fields_present {
        // 注音提取规则：数组（或 JSON 字符串）→ 校验（逐条编译正则）→ 存规范化 JSON
        if key == "tts_pinyin_rules" {
            let value = updates.get(key).cloned().unwrap_or(Value::Null);
            let rules = crate::services::tts::parse_pinyin_rules(&value)?;
            let normalized = serde_json::to_string(&rules).map_err(|e| {
                AppError::internal("tts-pinyin-rules-serialize", e)
            })?;
            settings_service::set_setting(&state.db, key, &normalized).await?;
            changed.insert(
                key.clone(),
                serde_json::to_value(&rules).unwrap_or(Value::Array(vec![])),
            );
            continue;
        }
        if key == "online_dict_proxy" {
            let value = save_online_dict_proxy(
                &state.db,
                updates.get(key).and_then(|v| v.as_str()),
                defaults,
            )
            .await?;
            changed.insert(key.clone(), Value::String(value));
            continue;
        }
        let known = BOOL_KEYS.contains(&key.as_str())
            || INT_KEYS.contains(&key.as_str())
            || OPTIONAL_INT_KEYS.contains(&key.as_str())
            || STR_KEYS.contains(&key.as_str());
        if !known {
            continue;
        }
        let value = updates.get(key).cloned().unwrap_or(Value::Null);
        if OPTIONAL_INT_KEYS.contains(&key.as_str()) {
            let stored = if value.is_null() {
                String::new()
            } else {
                value.as_i64().unwrap_or(0).to_string()
            };
            settings_service::set_setting(&state.db, key, &stored).await?;
        } else if BOOL_KEYS.contains(&key.as_str()) {
            let flag = value.as_bool().unwrap_or(false);
            settings_service::set_setting(&state.db, key, if flag { "true" } else { "false" }).await?;
        } else if INT_KEYS.contains(&key.as_str()) {
            settings_service::set_setting(&state.db, key, &value.as_i64().unwrap_or(0).to_string())
                .await?;
        } else {
            let mut text = value.as_str().unwrap_or_default().to_string();
            if key == "online_dict_sources" {
                text = normalize_online_sources(Some(&text));
            }
            settings_service::set_setting(&state.db, key, &text).await?;
        }
        changed.insert(key.clone(), value);
    }

    if !changed.is_empty() {
        audit_service::log_action(
            &state.db,
            "admin",
            Some(admin_id),
            "settings.update",
            None,
            Some(Value::Object(changed)),
        )
        .await?;
    }
    // 「随机浏览」从关改开时立即后台预热主键区间（对齐 Python：
    // 否则开启后第一个点随机的用户要付一次全区间扫描的代价）
    if warm_random {
        let state2 = state.clone();
        tokio::spawn(async move {
            crate::services::random_entry::warm_bounds_in_background(&state2).await;
        });
    }
    get_all_settings(&state.db, defaults)
        .await
        .map_err(AppError::from)
}

//! /api/admin/settings —— 移植自 `app/api/admin/settings.py`。

use actix_web::web;
use serde_json::Value;

use crate::core::deps::AdminAuth;
use crate::core::errors::AppError;
use crate::services::admin_settings_service;
use crate::AppState;

pub async fn get_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
) -> Result<web::Json<Value>, AppError> {
    Ok(web::Json(
        admin_settings_service::get_all_settings(&app.db, &app.cfg).await?,
    ))
}

/// 全字段可选；区分「未传」与「显式传 null」（fields_present 来自 body 的键集合）
pub async fn update_settings(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
    body: web::Json<Value>,
) -> Result<web::Json<Value>, AppError> {
    let Some(obj) = body.as_object() else {
        return Err(AppError::validation("请求体必须是 JSON 对象"));
    };
    let fields_present: Vec<String> = obj.keys().cloned().collect();
    Ok(web::Json(
        admin_settings_service::update_settings(&app, obj, &fields_present, &app.cfg, _admin.0.id)
            .await?,
    ))
}

#[derive(serde::Deserialize)]
pub struct PinyinRuleTestQuery {
    /// 待测规则（缺省 = 已保存的规则，方便「保存后再验一遍」）
    #[serde(default)]
    pub rules: Option<Value>,
    /// 样本文本（贴一段释义 HTML）
    pub text: String,
}

/// POST /api/admin/settings/tts-pinyin-rules/test —— 逐条跑注音提取规则，
/// 返回每条的命中与提取值。用与朗读路径完全相同的引擎（fancy-regex + 守门校验），
/// 前端所见即后端所得。
pub async fn test_pinyin_rules(
    app: web::Data<std::sync::Arc<AppState>>,
    _admin: AdminAuth,
    body: web::Json<PinyinRuleTestQuery>,
) -> Result<web::Json<Value>, AppError> {
    let rules = match &body.rules {
        Some(value) => crate::services::tts::parse_pinyin_rules(value)?,
        None => {
            let raw = crate::services::settings_service::get_setting(
                &app.db,
                "tts_pinyin_rules",
                Some(""),
            )
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
            crate::services::tts::parse_pinyin_rules(&Value::String(raw))?
        }
    };
    let mut out = Vec::with_capacity(rules.len());
    for rule in &rules {
        let compiled = match fancy_regex::Regex::new(&rule.pattern) {
            Ok(re) => re,
            Err(e) => {
                out.push(serde_json::json!({
                    "name": rule.name, "enabled": rule.enabled,
                    "matched": false, "value": null, "note": format!("编译失败：{e}"),
                }));
                continue;
            }
        };
        // 与正式提取同口径：第一个捕获组，无捕获组取整体
        let value = compiled
            .captures(&body.text)
            .ok()
            .flatten()
            .and_then(|caps| {
                caps.get(1).or_else(|| caps.get(0)).map(|m| m.as_str().trim().to_string())
            });
        out.push(serde_json::json!({
            "name": rule.name,
            "enabled": rule.enabled,
            "matched": value.is_some(),
            "value": value,
        }));
    }
    Ok(web::Json(serde_json::json!({ "results": out })))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/admin/settings", web::get().to(get_settings))
        .route("/admin/settings", web::put().to(update_settings))
        .route(
            "/admin/settings/tts-pinyin-rules/test",
            web::post().to(test_pinyin_rules),
        );
}

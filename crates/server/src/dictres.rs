//! GET /dict-res/{dictionary_id}/res/{path} —— 移植自 `app/main.py::dict_resource`。
//!
//! 只读对外暴露词典 res/ 子目录；source/ 原始文件不经此路由可达。
//! 必须带 Access-Control-Allow-Origin：词条 iframe（sandbox="allow-scripts"、不含
//! allow-same-origin）是不透明源，加载这里的 @font-face 与 XHR 都算跨域，没有这个头
//! 会**静默失败**。CSP sandbox 让 .html/.svg 被直接打开时也是不透明源（防 token 泄露）。

use actix_files::NamedFile;
use actix_web::{web, HttpRequest, HttpResponse};
use std::path::PathBuf;

/// 常量 MIME 字符串 → mime::Mime（只接受本文件里 resource_media_type 的静态值）
fn parse_mime(value: &str) -> mime::Mime {
    value
        .parse()
        .unwrap_or(mime::APPLICATION_OCTET_STREAM)
}

use dict_parser::resources as res;

use crate::AppState;

fn dict_res_headers(mut resp: HttpResponse) -> HttpResponse {
    use actix_web::http::header::{HeaderValue, ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL,
        CONTENT_SECURITY_POLICY, X_CONTENT_TYPE_OPTIONS};
    use actix_web::http::header::HeaderName;
    resp.headers_mut().insert(
        ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    // 嵌入方页面开了 COEP: require-corp 时，跨域子资源必须显式放行
    let coru: HeaderName = HeaderName::from_static("cross-origin-resource-policy");
    resp.headers_mut().insert(coru, HeaderValue::from_static("cross-origin"));
    resp.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
    resp.headers_mut()
        .insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox allow-scripts"));
    resp.headers_mut().insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

pub async fn dict_resource(
    app: web::Data<std::sync::Arc<AppState>>,
    req: HttpRequest,
    path: web::Path<(i32, String)>,
) -> HttpResponse {
    let (dictionary_id, resource_path) = path.into_inner();
    let Ok(normalized) = res::normalize_resource_path(&resource_path) else {
        return HttpResponse::NotFound().finish();
    };
    // 历史坏链接：早期改写把 file:///down/x.gif 拼成了 res/file:/down/x.gif
    let normalized = res::strip_legacy_file_prefix(&normalized);
    let res_dir = PathBuf::from(&app.cfg.dictionary_storage_path)
        .join(dictionary_id.to_string())
        .join("res");

    // 文件解析放阻塞线程（大小写不敏感兜底可能要建目录索引，30k 项 scandir）
    let res_dir2 = res_dir.clone();
    let normalized2 = normalized.clone();
    let target = tokio::task::spawn_blocking(move || {
        res::resolve_resource_file(&res_dir2, &normalized2)
    })
    .await
    .ok()
    .flatten();

    let target = match target {
        Some(target) => Some(target),
        // .mp3 缺而 .spx 在 → 现场转码
        None if normalized.to_lowercase().ends_with(".mp3") => {
            let spx_path = {
                let mut s = normalized.clone();
                s.truncate(s.len() - 4);
                s.push_str(".spx");
                s
            };
            let source = res::resolve_resource_file(&res_dir, &spx_path);
            match source {
                Some(source) => crate::services::spx::transcode_to_mp3(&source).await,
                None => None,
            }
        }
        None => None,
    };

    let Some(target) = target else {
        return HttpResponse::NotFound().finish();
    };
    let media_type = res::resource_media_type(&target);
    let named = NamedFile::open(&target);
    match named {
        Ok(file) => {
            let resp = file
                .disable_content_disposition()
                .set_content_type(parse_mime(media_type))
                .into_response(&req);
            dict_res_headers(resp)
        }
        Err(_) => HttpResponse::NotFound().finish(),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/dict-res/{dictionary_id}/res/{resource_path:.*}",
        web::get().to(dict_resource),
    );
}

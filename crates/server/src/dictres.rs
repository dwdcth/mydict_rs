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

    // 1) 磁盘 res/（兄弟文件/历史解包/转码产物）：大小写不敏感解析放阻塞线程
    let res_dir2 = res_dir.clone();
    let normalized2 = normalized.clone();
    let disk_target = tokio::task::spawn_blocking(move || {
        res::resolve_resource_file(&res_dir2, &normalized2)
    })
    .await
    .ok()
    .flatten();

    if let Some(target) = disk_target {
        let media_type = res::resource_media_type(&target);
        return match NamedFile::open(&target) {
            Ok(file) => dict_res_headers(
                file.disable_content_disposition()
                    .set_content_type(parse_mime(media_type))
                    .into_response(&req),
            ),
            Err(_) => HttpResponse::NotFound().finish(),
        };
    }

    // 2) 直接从词典的 .mdd 按需读取（磁盘优化：不再导入期全量解包）
    if let Some(bytes) = crate::services::mdd_resources::lookup_resource(
        &app,
        dictionary_id,
        &normalized,
    )
    .await
    {
        let media_type = res::resource_media_type(std::path::Path::new(&normalized));
        return dict_res_headers(
            HttpResponse::Ok()
                .content_type(parse_mime(media_type))
                .body(bytes.to_vec()),
        );
    }

    // 3) .mp3 缺而 .spx 在（res/ 或 .mdd）→ 现场转码
    if normalized.to_lowercase().ends_with(".mp3") {
        let spx_rel = format!("{}.spx", &normalized[..normalized.len() - 4]);
        let source = match res::resolve_resource_file(&res_dir, &spx_rel) {
            Some(path) => Some(path),
            None => {
                // 从 .mdd 读出 .spx 字节 → 落临时文件再转码
                match crate::services::mdd_resources::lookup_resource(&app, dictionary_id, &spx_rel).await {
                    Some(bytes) => {
                        let res_dir = res_dir.clone();
                        match tokio::task::spawn_blocking(move || -> std::io::Result<PathBuf> {
                            std::fs::create_dir_all(&res_dir)?;
                            let tmp = res_dir.join(format!(".spx-src-{}", std::process::id()));
                            std::fs::write(&tmp, bytes.as_slice())?;
                            Ok(tmp)
                        })
                        .await
                        {
                            Ok(Ok(tmp)) => Some(tmp),
                            _ => None,
                        }
                    }
                    None => None,
                }
            }
        };
        if let Some(source) = source {
            if let Some(mp3) = crate::services::spx::transcode_to_mp3(&source).await {
                let media_type = res::resource_media_type(&mp3);
                return match NamedFile::open(&mp3) {
                    Ok(file) => dict_res_headers(
                        file.disable_content_disposition()
                            .set_content_type(parse_mime(media_type))
                            .into_response(&req),
                    ),
                    Err(_) => HttpResponse::NotFound().finish(),
                };
            }
        }
    }

    HttpResponse::NotFound().finish()
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/dict-res/{dictionary_id}/res/{resource_path:.*}",
        web::get().to(dict_resource),
    );
}

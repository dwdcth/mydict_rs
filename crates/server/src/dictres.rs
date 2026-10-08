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

/// mdd 查找走候选链（原样优先，未命中依次试解码/simplified 变体）；
/// 首个命中的结果按**原始请求键**写入字节缓存，后续同键请求零查找
async fn lookup_resource_with_candidates(
    app: &std::sync::Arc<AppState>,
    dictionary_id: i32,
    normalized: &str,
) -> Option<std::sync::Arc<Vec<u8>>> {
    if let Some(hit) =
        crate::services::mdd_resources::lookup_resource(app, dictionary_id, normalized).await
    {
        return Some(hit);
    }
    for candidate in res::resource_candidates(normalized).into_iter().skip(1) {
        if let Some(hit) =
            crate::services::mdd_resources::lookup_resource(app, dictionary_id, &candidate).await
        {
            return Some(hit);
        }
    }
    None
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

    // 外部样式空响应：词典里引用的 googleapis 在线字体/样式 CSS 拿不到也不该 404
    // 刷屏（返回空 CSS，浏览器静默跳过）
    let lower = normalized.to_lowercase();
    if lower.contains("googleapis.") || lower == "googleapis.css" {
        return dict_res_headers(
            HttpResponse::Ok()
                .content_type("text/css")
                .body("/* external css not available offline */"),
        );
    }

    // .spx 直连：浏览器播不了 Ogg/Speex，解成 WAV 返回（缓存 x.spx.wav；
    // 解码失败则继续往下走原样返回——下载场景仍可用）
    if lower.ends_with(".spx") {
        if let Some(resp) = spx_as_wav_response(&app, &req, dictionary_id, &normalized).await {
            return resp;
        }
    }

    // 1) 磁盘 res/（兄弟文件/历史解包/转码产物）：大小写不敏感解析放阻塞线程。
    //    按候选链依次试：多次 percent-decode、simplified/ 前缀、_simplified 后缀
    //    （词典兼容容错——部分词典引用与 mdd 键的编码/目录约定不一致）
    let candidates = res::resource_candidates(&normalized);
    let res_dir2 = res_dir.clone();
    let candidates2 = candidates.clone();
    let disk_target = tokio::task::spawn_blocking(move || {
        for candidate in &candidates2 {
            if let Some(target) = res::resolve_resource_file(&res_dir2, candidate) {
                return Some(target);
            }
        }
        None
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

    // 2) 直接从词典的 .mdd 按需读取（磁盘优化：不再导入期全量解包）；
    //    同样按候选链试（MddHandle 内部还有 basename/后缀兜底）
    if let Some(bytes) = lookup_resource_with_candidates(&app, dictionary_id, &normalized).await
    {
        let media_type = res::resource_media_type(std::path::Path::new(&normalized));
        return dict_res_headers(
            HttpResponse::Ok()
                .content_type(parse_mime(media_type))
                .body(bytes.to_vec()),
        );
    }

    // 3) .mp3 缺而 .spx 在（res/ 或 .mdd）→ 纯 Rust 解码成 WAV 返回
    //    （前端播放器拿到的 Content-Type 是 audio/wav，URL 的 .mp3 后缀无所谓）
    if normalized.to_lowercase().ends_with(".mp3") {
        let spx_rel = format!("{}.spx", &normalized[..normalized.len() - 4]);
        if let Some(resp) = spx_as_wav_response(&app, &req, dictionary_id, &spx_rel).await {
            return resp;
        }
    }

    HttpResponse::NotFound().finish()
}

/// 把一条 .spx 资源按 WAV 返回（缓存名 `x.spx.wav`，不占真实 .wav 的名字空间）。
/// 缓存命中直接发文件；否则从 res/ 或 .mdd 取 .spx 字节 → 解码 → 尽力落盘。
/// 解码失败返回 None（调用方按 404 或原样回落处理）。
async fn spx_as_wav_response(
    app: &std::sync::Arc<AppState>,
    req: &HttpRequest,
    dictionary_id: i32,
    spx_rel: &str,
) -> Option<HttpResponse> {
    let res_dir = PathBuf::from(&app.cfg.dictionary_storage_path)
        .join(dictionary_id.to_string())
        .join("res");
    let wav_rel = format!("{spx_rel}.wav");

    // 1) 缓存命中（大小写不敏感链，与其他资源同口径）
    let res_dir2 = res_dir.clone();
    let wav_rel2 = wav_rel.clone();
    let cached = tokio::task::spawn_blocking(move || {
        dict_parser::resources::resource_candidates(&wav_rel2)
            .iter()
            .find_map(|c| dict_parser::resources::resolve_resource_file(&res_dir2, c))
    })
    .await
    .ok()
    .flatten();
    if let Some(path) = cached {
        return Some(match NamedFile::open(&path) {
            Ok(file) => dict_res_headers(
                file.disable_content_disposition()
                    .set_content_type(parse_mime("audio/wav"))
                    .into_response(req),
            ),
            Err(_) => HttpResponse::NotFound().finish(),
        });
    }

    // 2) 取 .spx 源字节：res/ 磁盘 → .mdd
    let spx_bytes = if let Some(path) =
        res::resolve_resource_file(&res_dir, spx_rel)
    {
        tokio::task::spawn_blocking(move || std::fs::read(path)).await.ok()?.ok()?
    } else {
        lookup_resource_with_candidates(app, dictionary_id, spx_rel).await?.to_vec()
    };

    // 3) 解码（纯 CPU）+ 尽力落盘缓存
    let wav = tokio::task::spawn_blocking(move || crate::services::spx::decode_spx_to_wav(&spx_bytes))
        .await
        .ok()?
        .ok()?;
    let res_dir3 = res_dir.clone();
    let wav_bytes = wav.clone();
    let written = tokio::task::spawn_blocking(move || {
        res::write_resource(&res_dir3, &wav_rel, &wav_bytes, false)
            .ok()
            .map(|_| true)
    })
    .await
    .ok()
    .flatten();
    if written == Some(true) {
        // 重开缓存文件（带 ETag/Last-Modified 等文件响应头）
        let candidates = res::resource_candidates(&format!("{spx_rel}.wav"));
        for candidate in &candidates {
            if let Some(path) = res::resolve_resource_file(&res_dir, candidate) {
                return Some(match NamedFile::open(&path) {
                    Ok(file) => dict_res_headers(
                        file.disable_content_disposition()
                            .set_content_type(parse_mime("audio/wav"))
                            .into_response(req),
                    ),
                    Err(_) => HttpResponse::NotFound().finish(),
                });
            }
        }
    }
    Some(dict_res_headers(
        HttpResponse::Ok()
            .content_type(parse_mime("audio/wav"))
            .body(wav),
    ))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/dict-res/{dictionary_id}/res/{resource_path:.*}",
        web::get().to(dict_resource),
    );
}

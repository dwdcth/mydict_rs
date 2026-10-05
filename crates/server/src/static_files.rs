//! 前端静态托管与 SPA fallback —— 移植自 `app/main.py` 尾部。
//!
//! 仅当 `{static_dir}/assets` 存在（打包产物就位）时挂载：
//! - `/assets/*` 与其它哈希文件直接返回（ETag/Last-Modified 由文件服务提供）
//! - 其余路径回退 `index.html` 且 `Cache-Control: no-cache`——它没有哈希名，
//!   被启发式缓存会让用户部署后还跑上一版 JS

use actix_files::NamedFile;
use actix_web::{web, HttpRequest, HttpResponse};
use std::path::{Path, PathBuf};

pub fn configure(cfg: &mut web::ServiceConfig) {
    // static_dir 是进程级配置，configure 时从环境拿一次（与 Python 启动时判定等价）
    let static_dir = PathBuf::from(
        std::env::var("STATIC_DIR").unwrap_or_else(|_| "static".to_string()),
    );
    if !(static_dir.join("assets")).is_dir() {
        return;
    }
    let assets = static_dir.join("assets");
    cfg.app_data(web::Data::new(StaticDir(static_dir.clone())))
        .service(actix_files::Files::new("/assets", assets))
        .route("/{full_path:.*}", web::get().to(spa_fallback));
}

#[derive(Clone)]
pub struct StaticDir(pub PathBuf);

async fn spa_fallback(
    req: HttpRequest,
    path: web::Path<String>,
    static_dir: web::Data<StaticDir>,
) -> HttpResponse {
    let full_path = path.into_inner();
    if let Some(candidate) = safe_join(&static_dir.0, &full_path) {
        if candidate.is_file() {
            // 哈希命名的静态资源可以放心长缓存（对齐 Python：FileResponse 默认头）
            return NamedFile::open(candidate)
                .map(|f| f.into_response(&req))
                .unwrap_or_else(|_| HttpResponse::NotFound().finish());
        }
    }
    let index = static_dir.0.join("index.html");
    if !index.is_file() {
        return HttpResponse::NotFound().finish();
    }
    let mut resp = NamedFile::open(index)
        .map(|f| f.disable_content_disposition().into_response(&req))
        .unwrap_or_else(|_| HttpResponse::NotFound().finish());
    // index.html 本身没有哈希，必须 no-cache（原注释：否则浏览器启发式缓存旧页面）
    resp.headers_mut().insert(
        actix_web::http::header::CACHE_CONTROL,
        actix_web::http::header::HeaderValue::from_static("no-cache"),
    );
    resp
}

/// 拼接并拒绝越出 static_dir 的路径（防 ../ 穿越）
fn safe_join(base: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return None;
    }
    let joined = base.join(rel);
    let canonical_base = base.canonicalize().ok()?;
    let canonical = joined.canonicalize().ok()?;
    if canonical.starts_with(&canonical_base) {
        Some(canonical)
    } else {
        None
    }
}

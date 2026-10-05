//! 版本号 —— 移植自 `app/core/version.py`：读版本文件（构建时写入 GIT_BRANCH），
//! 缺失/空回落 "dev"。

use std::sync::OnceLock;

const DEFAULT_VERSION: &str = "dev";

pub fn get_app_version(version_file_path: &str) -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| match std::fs::read_to_string(version_file_path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                DEFAULT_VERSION.to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(_) => DEFAULT_VERSION.to_string(),
    })
}

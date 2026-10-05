//! 环境变量配置 —— 移植自 Python 版 `app/core/config.py`（pydantic-settings）。
//!
//! 字段名与 env 变量名一一对应（小写下划线）。新增：
//! - `database_url`：多后端支持（SeaORM），默认从 `database_path` 拼 sqlite URL
//! - `static_dir`：前端构建产物目录（原版是 `app/static` 相对路径）

use std::path::Path;

#[derive(Debug, Clone)]
pub struct Settings {
    pub jwt_secret: String,
    pub open_access_default: bool,
    pub allow_registration_default: bool,
    pub token_default_daily_limit: i64,
    pub anonymous_ip_rate_limit_per_min: i64,
    pub user_ip_rate_limit_per_min: i64,
    pub max_upload_size_mb: i64,
    pub enable_scheduler: bool,
    /// 「一天」的划分时区；留空或无效时回落系统时区
    pub timezone: String,
    pub version_file_path: String,

    pub config_storage_path: String,
    pub dicts_inbox_path: String,
    pub database_path: String,
    /// 非空则优先（支持 postgres:// / mysql://）；空则用 database_path 的 sqlite
    pub database_url: String,
    pub dictionary_storage_path: String,
    pub log_dir: String,
    pub static_dir: String,

    /// 在线词典出站抓取（维基百科/维基词典）用的 HTTP 代理，留空直连；管理后台可覆盖
    pub online_dict_proxy: String,
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// bool 解析对齐 pydantic-settings：true/1/yes/on（大小写不敏感）
fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"),
        Err(_) => default,
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            jwt_secret: env_str("JWT_SECRET", ""),
            open_access_default: env_bool("OPEN_ACCESS_DEFAULT", false),
            allow_registration_default: env_bool("ALLOW_REGISTRATION_DEFAULT", true),
            token_default_daily_limit: env_i64("TOKEN_DEFAULT_DAILY_LIMIT", 1000),
            anonymous_ip_rate_limit_per_min: env_i64("ANONYMOUS_IP_RATE_LIMIT_PER_MIN", 60),
            user_ip_rate_limit_per_min: env_i64("USER_IP_RATE_LIMIT_PER_MIN", 120),
            max_upload_size_mb: env_i64("MAX_UPLOAD_SIZE_MB", 512),
            enable_scheduler: env_bool("ENABLE_SCHEDULER", true),
            timezone: env_str("TIMEZONE", ""),
            version_file_path: env_str("VERSION_FILE_PATH", "/version.txt"),
            config_storage_path: env_str("CONFIG_STORAGE_PATH", "/data/config"),
            dicts_inbox_path: env_str("DICTS_INBOX_PATH", "/data/dicts"),
            database_path: env_str("DATABASE_PATH", "/data/db/mydict.sqlite3"),
            database_url: env_str("DATABASE_URL", ""),
            dictionary_storage_path: env_str("DICTIONARY_STORAGE_PATH", "/data/dictionaries"),
            log_dir: env_str("LOG_DIR", "/data/logs"),
            static_dir: env_str("STATIC_DIR", "static"),
            online_dict_proxy: env_str("ONLINE_DICT_PROXY", ""),
        }
    }

    /// 实际使用的数据库 URL：DATABASE_URL 优先，否则 sqlite:///{database_path}
    pub fn database_url(&self) -> String {
        if !self.database_url.is_empty() {
            self.database_url.clone()
        } else {
            format!("sqlite:///{}", self.database_path)
        }
    }

    pub fn is_sqlite(&self) -> bool {
        self.database_url().starts_with("sqlite")
    }

    pub fn ensure_data_dirs(&self) -> std::io::Result<()> {
        let db_parent = Path::new(&self.database_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        for path in [
            Path::new(&self.config_storage_path),
            Path::new(&self.dicts_inbox_path),
            &db_parent,
            Path::new(&self.dictionary_storage_path),
            Path::new(&self.log_dir),
        ] {
            std::fs::create_dir_all(path)?;
        }
        Ok(())
    }
}

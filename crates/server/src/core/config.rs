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
    /// zip 上传解压后的总容量上限（压缩包解开后常远超上传体积，与 MAX_UPLOAD_SIZE_MB
    /// 独立；默认 8GB——真实词库的 .mdd 动辄数 GB）
    pub upload_unpack_limit_mb: i64,
    /// 全量解析（导入/lite→full 转换）的并行 worker 数。
    /// 0 = 自动：核数一半、封顶 4（CPU 留余量，导入任务又经 bulk_write 串行排队）
    pub import_workers: usize,
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

    /// .mdd 句柄缓存（词头索引常驻内存）的容量预算，按 mdictlib memory_usage 实测字节计重。
    /// 对齐 GoldenDict/MDict 的「按需打开 + 限量常驻」：预算内 LRU 常驻，超了淘汰最久未用
    pub dict_mdd_handle_budget_mb: usize,
    /// lite 词典解析器句柄（mdx 词头索引）缓存的容量预算，按词条数 × 32B 估算计重
    pub dict_mdx_handle_budget_mb: usize,
    /// 词典句柄/资源缓存的空闲回收秒数：超过此时长未被查询的词典关闭并释放内存
    /// （下次查询重新打开，毫秒到秒级；GoldenDict 靠手动分组，服务端用空闲超时自动做）
    pub dict_idle_unload_secs: u64,
}

fn env_str(key: &str, default: &str) -> String {
    env_value(key).unwrap_or_else(|| default.to_string())
}

/// bool 解析对齐 pydantic-settings：true/1/yes/on（大小写不敏感）
/// cwd 的 .env 解析结果（对齐 pydantic-settings 的 env_file=".env"；
/// 进程环境变量优先，.env 只垫底缺失项）。启动期一次解析、全局只读。
fn dotenv_map() -> &'static std::collections::HashMap<String, String> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let Ok(content) = std::fs::read_to_string(".env") else {
            return std::collections::HashMap::new();
        };
        let mut map = std::collections::HashMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let mut value = value.trim().to_string();
            if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                value = value[1..value.len() - 1].to_string();
            }
            if !key.is_empty() {
                map.entry(key.to_string()).or_insert(value);
            }
        }
        map
    })
}

/// 环境取值：进程 env 优先，其次 .env
fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().or_else(|| dotenv_map().get(key).cloned())
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_value(key).as_deref() {
        // pydantic 布尔解析集合：y/yes/t/true/on/1（大小写不敏感）
        Some(v) => matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on" | "y" | "t"
        ),
        None => default,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    env_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 0 = 自动：核数一半、封顶 4（CPU 留余量，多词典导入也不叠满核）
fn effective_import_workers(configured: usize) -> usize {
    if configured > 0 {
        return configured.min(16);
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 4)
}

fn env_i64(key: &str, default: i64) -> i64 {
    env_value(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Settings {
    pub fn from_env() -> Self {
        let mut settings = Self {
            jwt_secret: env_str("JWT_SECRET", ""),
            open_access_default: env_bool("OPEN_ACCESS_DEFAULT", false),
            allow_registration_default: env_bool("ALLOW_REGISTRATION_DEFAULT", true),
            token_default_daily_limit: env_i64("TOKEN_DEFAULT_DAILY_LIMIT", 1000),
            anonymous_ip_rate_limit_per_min: env_i64("ANONYMOUS_IP_RATE_LIMIT_PER_MIN", 60),
            user_ip_rate_limit_per_min: env_i64("USER_IP_RATE_LIMIT_PER_MIN", 120),
            max_upload_size_mb: env_i64("MAX_UPLOAD_SIZE_MB", 512),
            import_workers: env_usize("IMPORT_WORKERS", 0),
            upload_unpack_limit_mb: env_i64("UPLOAD_UNPACK_LIMIT_MB", 8 * 1024),
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
            dict_mdd_handle_budget_mb: env_usize("DICT_MDD_HANDLE_BUDGET_MB", 384),
            dict_mdx_handle_budget_mb: env_usize("DICT_MDX_HANDLE_BUDGET_MB", 384),
            dict_idle_unload_secs: env_u64("DICT_IDLE_UNLOAD_SECS", 1800),
        };
        settings.import_workers = effective_import_workers(settings.import_workers);
        settings
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

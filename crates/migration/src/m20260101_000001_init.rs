use sea_orm::DbBackend;
use sea_orm_migration::prelude::*;

/// 初始 schema —— 重新设计版（不兼容 Python 版数据）。
///
/// 与 Python 版的差异（设计文档「schema 修复 10 项」）：
/// 1. 时间戳 INTEGER unix 秒（原 TEXT naive-UTC）
/// 2. allowed_dictionary_ids JSON 列 → 关联表 + `*_limited` 布尔列（原三处 JSON 无外键完整性）
/// 3. token_plain 明文 → token_secret AES-GCM 密文
/// 4. FK 统一：强所有权 CASCADE、统计引用 SET NULL，FK 列全部有索引
/// 5. query_stats_daily 一表两用 → token_usage_daily（限流计数）+ stats_daily（聚合）
/// 6. file_path "; " 拼接列 → dictionary_sources 子表
/// 7. dict_entries 覆盖索引带上 generation；「代」机制保留
/// 8. 生词本部分唯一索引（MySQL 无部分索引，应用层兜底）
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DDL).await?;
        seed_settings(manager).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}

/// 全库 DDL 用原生 SQL（SQLite/PostgreSQL 语法交集；MySQL 见下方注释）。
/// 选原生 SQL 而非 sea-query builder 的原因：部分唯一索引、CHECK 约束、
/// 复合主键关联表这些细节用 builder 表达冗长且方言开关更多，SQL 反而最直白。
///
/// MySQL 部署时需人工调整：INTEGER→BIGINT（无碍，MySQL INTEGER 即 32 位）、
/// 部分唯一索引不支持（删除两个 WHERE 子句索引，服务层已有同语义兜底）。
/// 当前验收后端只有 SQLite，这里保持单一 DDL 简单可靠。
const DDL: &str = r#"
CREATE TABLE admins (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  username        TEXT NOT NULL UNIQUE,
  password_hash   TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  last_login_at   INTEGER
);

CREATE TABLE dictionaries (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  name              TEXT NOT NULL,
  format            TEXT NOT NULL CHECK (format IN ('mdict','stardict','ecdict')),
  lang_from         TEXT NOT NULL,
  lang_to           TEXT NOT NULL,
  import_method     TEXT NOT NULL DEFAULT 'dicts_dir' CHECK (import_method IN ('upload','dicts_dir')),
  word_count        INTEGER NOT NULL DEFAULT 0,
  sort_order        INTEGER NOT NULL DEFAULT 0,
  active_generation INTEGER NOT NULL DEFAULT 0,
  status            TEXT NOT NULL DEFAULT 'disabled' CHECK (status IN ('enabled','disabled')),
  imported_at       INTEGER NOT NULL,
  imported_by       INTEGER REFERENCES admins(id) ON DELETE SET NULL
);

CREATE TABLE users (
  id                  INTEGER PRIMARY KEY AUTOINCREMENT,
  username            TEXT NOT NULL UNIQUE,
  email               TEXT UNIQUE,
  password_hash       TEXT NOT NULL,
  status              TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
  created_at          INTEGER NOT NULL,
  last_login_at       INTEGER,
  admin_scope_limited INTEGER NOT NULL DEFAULT 0,
  self_scope_limited  INTEGER NOT NULL DEFAULT 0
);

-- 管理员划定的「可用词典」上限；无行 = 不限制（admin_scope_limited=0）
CREATE TABLE admin_dict_grants (
  user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, dictionary_id)
);
CREATE INDEX ix_admin_dict_grants_dict ON admin_dict_grants (dictionary_id);

-- 用户在上限内的自选
CREATE TABLE self_dict_grants (
  user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, dictionary_id)
);
CREATE INDEX ix_self_dict_grants_dict ON self_dict_grants (dictionary_id);

-- 源文件清单（Python 版塞在 file_path 一列里用 "; " 拼接）
CREATE TABLE dictionary_sources (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
  position      INTEGER NOT NULL,
  path          TEXT NOT NULL,
  UNIQUE (dictionary_id, position)
);

CREATE TABLE dict_entries (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
  word          TEXT NOT NULL,
  word_lower    TEXT NOT NULL,
  phonetic      TEXT,
  definition    TEXT NOT NULL,
  extra         TEXT,
  generation    INTEGER NOT NULL DEFAULT 0
);
-- 覆盖索引：所有读路径都按 (dictionary_id, generation=active, word_lower...) 过滤
CREATE INDEX ix_dict_entries_dict_gen_word ON dict_entries (dictionary_id, generation, word_lower);
-- 全局按词跨词典（suggest 等场景）
CREATE INDEX ix_dict_entries_word_lower ON dict_entries (word_lower);

CREATE TABLE api_tokens (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  name           TEXT NOT NULL,
  token_hash     TEXT NOT NULL UNIQUE,
  token_prefix   TEXT NOT NULL,
  daily_limit    INTEGER,
  status         TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled')),
  created_at     INTEGER NOT NULL,
  created_by     INTEGER REFERENCES admins(id) ON DELETE SET NULL,
  last_used_at   INTEGER,
  user_id        INTEGER UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  scope_limited  INTEGER NOT NULL DEFAULT 0,
  token_secret   BLOB
);
CREATE INDEX ix_api_tokens_user ON api_tokens (user_id);

CREATE TABLE token_dict_grants (
  token_id      INTEGER NOT NULL REFERENCES api_tokens(id) ON DELETE CASCADE,
  dictionary_id INTEGER NOT NULL REFERENCES dictionaries(id) ON DELETE CASCADE,
  PRIMARY KEY (token_id, dictionary_id)
);
CREATE INDEX ix_token_dict_grants_dict ON token_dict_grants (dictionary_id);

CREATE TABLE query_logs (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  source        TEXT NOT NULL CHECK (source IN ('api','web')),
  token_id      INTEGER REFERENCES api_tokens(id) ON DELETE SET NULL,
  user_id       INTEGER REFERENCES users(id) ON DELETE SET NULL,
  word          TEXT NOT NULL,
  dictionary_id INTEGER REFERENCES dictionaries(id) ON DELETE SET NULL,
  ip            TEXT,
  status        TEXT,
  duration_ms   INTEGER,
  created_at    INTEGER NOT NULL
);
CREATE INDEX ix_query_logs_created ON query_logs (created_at);
CREATE INDEX ix_query_logs_user ON query_logs (user_id);
CREATE INDEX ix_query_logs_token ON query_logs (token_id);
CREATE INDEX ix_query_logs_word ON query_logs (word);

-- Token 每日限流计数器（原版与聚合表一表两用，写入路径互相干扰，拆开）
CREATE TABLE token_usage_daily (
  stat_date           TEXT NOT NULL,
  token_id            INTEGER NOT NULL REFERENCES api_tokens(id) ON DELETE CASCADE,
  query_count         INTEGER NOT NULL DEFAULT 0,
  rate_limited_count  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (stat_date, token_id)
);

-- 用户/匿名维度日聚合（定时任务写入）
CREATE TABLE stats_daily (
  id                 INTEGER PRIMARY KEY AUTOINCREMENT,
  stat_date          TEXT NOT NULL,
  owner_kind         TEXT NOT NULL CHECK (owner_kind IN ('user','anon')),
  user_id            INTEGER REFERENCES users(id) ON DELETE SET NULL,
  query_count        INTEGER NOT NULL DEFAULT 0,
  rate_limited_count INTEGER NOT NULL DEFAULT 0,
  UNIQUE (stat_date, owner_kind, user_id)
);
CREATE INDEX ix_stats_daily_user ON stats_daily (user_id);

CREATE TABLE vocab_items (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id         INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  word            TEXT NOT NULL,
  dictionary_id   INTEGER REFERENCES dictionaries(id) ON DELETE SET NULL,
  dictionary_name TEXT,
  phonetic        TEXT,
  definition      TEXT,
  note            TEXT,
  created_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX uq_vocab_items_user_dict_word
  ON vocab_items (user_id, dictionary_id, word) WHERE dictionary_id IS NOT NULL;
CREATE INDEX ix_vocab_items_user_created ON vocab_items (user_id, created_at);

CREATE TABLE token_vocab_items (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  token_id        INTEGER NOT NULL REFERENCES api_tokens(id) ON DELETE CASCADE,
  word            TEXT NOT NULL,
  dictionary_id   INTEGER REFERENCES dictionaries(id) ON DELETE SET NULL,
  dictionary_name TEXT,
  phonetic        TEXT,
  definition      TEXT,
  note            TEXT,
  created_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX uq_token_vocab_items_token_dict_word
  ON token_vocab_items (token_id, dictionary_id, word) WHERE dictionary_id IS NOT NULL;
CREATE INDEX ix_token_vocab_items_token_created ON token_vocab_items (token_id, created_at);

CREATE TABLE audit_logs (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  actor_type TEXT NOT NULL CHECK (actor_type IN ('admin','system')),
  actor_id   INTEGER,
  action     TEXT NOT NULL,
  target     TEXT,
  detail     TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX ix_audit_logs_created ON audit_logs (created_at);

CREATE TABLE system_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT,
  updated_at INTEGER NOT NULL
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS system_settings;
DROP TABLE IF EXISTS audit_logs;
DROP TABLE IF EXISTS token_vocab_items;
DROP TABLE IF EXISTS vocab_items;
DROP TABLE IF EXISTS stats_daily;
DROP TABLE IF EXISTS token_usage_daily;
DROP TABLE IF EXISTS query_logs;
DROP TABLE IF EXISTS token_dict_grants;
DROP TABLE IF EXISTS api_tokens;
DROP TABLE IF EXISTS dict_entries;
DROP TABLE IF EXISTS dictionary_sources;
DROP TABLE IF EXISTS self_dict_grants;
DROP TABLE IF EXISTS admin_dict_grants;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS dictionaries;
DROP TABLE IF EXISTS admins;
"#;

/// 系统设置种子 —— key 与 Python 版完全一致（外加新增的 query_log_retention_days）。
/// 语义：env 是默认值，DB 行是覆盖；这里只播种「与 env 默认可能不同」的核心项，
/// 其余键由 settings_service 读时回落默认（与 Python 版一致：只有被修改过的才落行）。
async fn seed_settings(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let seeds: &[(&str, &str)] = &[
        ("open_access", "false"),
        ("allow_registration", "true"),
        ("online_dict_enabled", "false"),
        ("random_browse_enabled", "false"),
        ("token_default_daily_limit", "1000"),
        ("anonymous_ip_rate_limit_per_min", "60"),
        ("user_ip_rate_limit_per_min", "120"),
        // 新增：query_logs 保留期（空 = 永久），Python 版无清理机制的修复
        ("query_log_retention_days", ""),
    ];
    for (key, value) in seeds {
        let sql = match manager.get_database_backend() {
            DbBackend::MySql => format!(
                "INSERT IGNORE INTO system_settings (key, value, updated_at) VALUES ('{key}', '{value}', {now})"
            ),
            _ => format!(
                "INSERT INTO system_settings (key, value, updated_at) VALUES ('{key}', '{value}', {now}) ON CONFLICT(key) DO NOTHING"
            ),
        };
        manager.get_connection().execute_unprepared(&sql).await?;
    }
    Ok(())
}

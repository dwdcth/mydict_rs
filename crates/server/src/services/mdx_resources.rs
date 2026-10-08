//! 轻量挂载（lite）模式：释义按需从源文件物化 —— 与 `mdd_resources` 对偶的「mdx 侧」。
//!
//! lite 导入只落词头 + `source_ordinal`；查询期命中远程行时：
//! 1. 按词典缓存解析器实例（打开 = 加载词头索引，probe 路由/样式表复用导入管线）；
//!    缓存**按字节预算** LRU（词条数 × 32B 估算，对齐 mdictlib locator 实测 ~28B/条），
//!    且带**空闲回收**（DICT_IDLE_UNLOAD_SECS 未查询即关句柄释放内存）
//! 2. `definition_at(ordinal)` 单条读取（只解压所在 record 块）
//! 3. 与全量导入同一条处理管线：样式展开 → 资源引用改写（skip_resource_rewrite 词典除外）
//! 4. 释义缓存（64MB，同样空闲回收）——热词二次命中零解压
//!
//! @@@LINK 解引用也建立在物化之上：链接行的目标释义同样按需读取（链式 ≤5 层，
//! 每跳一次缓存读取），无需在导入期落 link_target 列。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache as MokaCache;
use sea_orm::{ConnectionTrait, Statement};

use crate::AppState;

/// 释义缓存上限（按 UTF-8 字节数计重）
const DEFINITION_CACHE: usize = 64 * 1024 * 1024;
/// 每词条词头索引常驻估算（mdictlib locator 实测 ~28B/条，取整留余量；
/// opendict v3 是 Vec<String> 模型更费，按同值估）
const BYTES_PER_ENTRY: u32 = 32;
/// 未落词条数（word_count=0）时的计重下限
const PARSER_WEIGHT_FLOOR: u32 = 256 * 1024;

enum RemoteParser {
    Mdict {
        files: Vec<PathBuf>,
        parser: std::sync::Mutex<dict_parser::mdict::MdictParser>,
        /// false = 对齐导入期 skip_resources：保留 sound:// 等原文不改写
        rewrite: bool,
    },
    StarDict {
        files: Vec<PathBuf>,
        parser: std::sync::Mutex<dict_parser::stardict::StarDictParser>,
    },
}

/// 缓存值：解析器 + 打开后的词头索引常驻估算（weigher 用）
struct CachedParser {
    parser: RemoteParser,
    weight: u32,
}

/// 词头索引常驻内存估算（导入期统计的 word_count × 每条开销）
fn parser_weight(word_count: i64) -> u32 {
    if word_count <= 0 {
        PARSER_WEIGHT_FLOOR
    } else {
        (BYTES_PER_ENTRY as u64)
            .saturating_mul(word_count as u64)
            .min(u32::MAX as u64) as u32
    }
}

pub struct DefinitionResources {
    parsers: MokaCache<i32, Arc<CachedParser>>,
    defs: MokaCache<String, Arc<String>>,
    /// 构建句柄时的预算（超过单词典体量时记 warn：每次查询都会重开，提示调预算）
    budget: usize,
}

impl DefinitionResources {
    /// `handle_budget`：句柄缓存字节预算；`idle`：空闲回收时长
    pub fn new(handle_budget: usize, idle: Duration) -> Self {
        Self {
            parsers: MokaCache::builder()
                .max_capacity(handle_budget as u64)
                .weigher(|_k, v: &Arc<CachedParser>| v.weight)
                .time_to_idle(idle)
                .build(),
            defs: MokaCache::builder()
                .max_capacity(DEFINITION_CACHE as u64)
                .weigher(|_k, v: &Arc<String>| v.len() as u32)
                .time_to_idle(idle)
                .build(),
            budget: handle_budget,
        }
    }

    /// 词典删除/重解析后失效（重解析可能换源文件、序号错位；全清重建成本可接受）
    pub fn invalidate_all(&self) {
        self.parsers.invalidate_all();
        self.defs.invalidate_all();
    }

    /// 触发 moka 维护任务：真正释放空闲过期条目占的内存（调度器周期调用）
    pub fn run_maintenance(&self) {
        self.parsers.run_pending_tasks();
        self.defs.run_pending_tasks();
    }

    /// 常驻内存估算合计（系统状态的观察口：句柄权重 + 释义缓存字节）。
    /// 先跑一次维护：insert 后未处理的条目对 iter() 不可见，直接读会少计
    pub fn resident_bytes(&self) -> u64 {
        self.parsers.run_pending_tasks();
        self.defs.run_pending_tasks();
        self.parsers.iter().map(|(_, v)| v.weight as u64).sum::<u64>()
            + self.defs.iter().map(|(_, v)| v.len() as u64).sum::<u64>()
    }
}

/// 取（或构建）某 lite 词典的远程解析句柄。命中缓存时零 DB 查询。
async fn remote_parser(state: &AppState, dictionary_id: i32) -> Option<Arc<CachedParser>> {
    if let Some(hit) = state.definition_resources.parsers.get(&dictionary_id) {
        return Some(hit);
    }
    let backend = state.db.get_database_backend();
    let dict_row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT format, skip_resource_rewrite, word_count FROM dictionaries WHERE id = $1",
            [dictionary_id.into()],
        ))
        .await
        .ok()
        .flatten()?;
    let format = dict_row.try_get::<String>("", "format").ok()?;
    let skip_rewrite: bool = dict_row
        .try_get::<Option<i32>>("", "skip_resource_rewrite")
        .ok()
        .flatten()
        .map(|v| v != 0)
        .unwrap_or(false);
    let word_count: i64 = dict_row
        .try_get::<i64>("", "word_count")
        .unwrap_or_default();
    let sources = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT path FROM dictionary_sources WHERE dictionary_id = $1 ORDER BY position",
            [dictionary_id.into()],
        ))
        .await
        .ok()?;
    let files: Vec<PathBuf> = sources
        .iter()
        .filter_map(|r| r.try_get::<String>("", "path").ok())
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .collect();
    if files.is_empty() {
        return None;
    }
    let parser = match format.as_str() {
        "mdict" => RemoteParser::Mdict {
            files,
            parser: std::sync::Mutex::new(dict_parser::mdict::MdictParser::new()),
            rewrite: !skip_rewrite,
        },
        "stardict" => RemoteParser::StarDict {
            files,
            parser: std::sync::Mutex::new(dict_parser::stardict::StarDictParser::new()),
        },
        // ECDICT 不支持 lite（导入期已拦截）
        _ => return None,
    };
    let weight = parser_weight(word_count);
    if weight as usize > state.definition_resources.budget {
        tracing::warn!(
            dictionary_id,
            word_count,
            budget_mb = state.definition_resources.budget / (1024 * 1024),
            "单部 lite 词典的词头索引估算超过句柄预算，每次查询都会重开（建议调大 DICT_MDX_HANDLE_BUDGET_MB）"
        );
    }
    let arc = Arc::new(CachedParser { parser, weight });
    state
        .definition_resources
        .parsers
        .insert(dictionary_id, arc.clone());
    Some(arc)
}

/// 物化一条远程释义：样式展开 + 资源改写后的最终 HTML（同全量导入落库内容）。
/// 源文件缺失/格式不符返回 None（查询层回退为空释义）。
pub async fn materialize_definition(
    state: &AppState,
    dictionary_id: i32,
    ordinal: i64,
) -> Option<Arc<String>> {
    let key = format!("{dictionary_id}:{ordinal}");
    if let Some(hit) = state.definition_resources.defs.get(&key) {
        return Some(hit);
    }
    let parser = remote_parser(state, dictionary_id).await?;
    // 打开 mdx（首个请求加载词头索引）与块解压都是阻塞 IO，放阻塞线程
    let text = tokio::task::spawn_blocking(move || -> Option<String> {
        match &parser.parser {
            RemoteParser::Mdict {
                files,
                parser,
                rewrite,
            } => {
                let mut p = parser.lock().ok()?;
                let rewrite_with = if *rewrite { Some(dictionary_id) } else { None };
                p.definition_at(files, ordinal, rewrite_with).ok()
            }
            RemoteParser::StarDict { files, parser } => {
                let mut p = parser.lock().ok()?;
                p.definition_at(files, ordinal).ok()
            }
        }
    })
    .await
    .ok()
    .flatten()?;
    let arc = Arc::new(text);
    state.definition_resources.defs.insert(key, arc.clone());
    Some(arc)
}

/// 预热：打开词典源文件并把句柄放进缓存（bootstrap/启用后调用，
/// 消掉重启后第一次查询的冷启动——大词典词头索引加载要几秒）
pub async fn warm_dictionary(state: &AppState, dictionary_id: i32) {
    let _ = materialize_definition(state, dictionary_id, 0).await;
}

/// 后台预热全部启用的 lite 词典（顺序执行，避免同时打开多个大文件把 IO 打满）
pub async fn warm_enabled_lite_dictionaries(state: &AppState) {
    use sea_orm::ConnectionTrait;
    let Ok(rows) = state
        .db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT id FROM dictionaries WHERE status = 'enabled' AND entry_mode = 'lite' \
             AND format IN ('mdict', 'stardict')",
            [],
        ))
        .await
    else {
        return;
    };
    for row in rows {
        let id: i32 = row.try_get("", "id").unwrap_or_default();
        warm_dictionary(state, id).await;
    }
}

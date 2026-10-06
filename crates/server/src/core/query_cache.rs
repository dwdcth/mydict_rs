//! 查询结果缓存 —— 移植自 `app/core/query_cache.py`。
//!
//! 进程内 TTL 缓存（300s / 10k 条），key = word_lower|候选词典集合|x{版本}|d?|a?。
//! 词典启停/导入/删除/改名/语言变更 → 全量失效。

use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache as MokaCache;
use serde_json::Value;

/// 变体展开规则版本：规则变更时递增，旧缓存 key 自然失效
/// （Python EXPANSION_VERSION=2）
pub const EXPANSION_VERSION: i32 = 2;

pub struct QueryCache {
    inner: MokaCache<String, Arc<Value>>,
    /// 词条文档 HTML 的短 TTL 缓存（图片版词典一份文档 35KB+，渲染含物化与
    /// mdd 资产探测；120s 过期，invalidate() 连带清空）
    docs: MokaCache<String, Arc<String>>,
}

impl QueryCache {
    pub fn new() -> Self {
        Self {
            inner: MokaCache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            docs: MokaCache::builder()
                .max_capacity(256)
                .time_to_live(Duration::from_secs(120))
                .build(),
        }
    }

    pub fn get_doc(&self, key: &str) -> Option<Arc<String>> {
        self.docs.get(key)
    }

    pub fn insert_doc(&self, key: String, body: Arc<String>) {
        self.docs.insert(key, body);
    }

    pub fn make_key(
        word_lower: &str,
        candidate_dict_ids: &[i32],
        include_definitions: bool,
        all_langs: bool,
    ) -> String {
        let mut ids: Vec<i32> = candidate_dict_ids.to_vec();
        ids.sort_unstable();
        let ids_str = ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{word_lower}|{ids_str}|x{EXPANSION_VERSION}|d{}|a{}",
            include_definitions as i32, all_langs as i32
        )
    }

    pub fn get(&self, key: &str) -> Option<Arc<Value>> {
        self.inner.get(key)
    }

    pub fn insert(&self, key: String, value: Arc<Value>) {
        self.inner.insert(key, value);
    }

    /// 全量失效（对齐 Python invalidate()）
    pub fn invalidate(&self) {
        self.docs.invalidate_all();
        self.inner.invalidate_all();
    }
}

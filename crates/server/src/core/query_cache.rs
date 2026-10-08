//! 查询结果缓存 —— 移植自 `app/core/query_cache.py`。
//!
//! 进程内 TTL 缓存（300s / 64MB，按序列化后体积计重——大结果会挤掉小结果，
//! 词典多、释义大时内存有界），key = word_lower|候选词典集合|x{版本}|d?|a?。
//! 词典启停/导入/删除/改名/语言变更 → 全量失效。

use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache as MokaCache;
use serde_json::Value;

/// 变体展开规则版本：规则变更时递增，旧缓存 key 自然失效
/// （Python EXPANSION_VERSION=2）
pub const EXPANSION_VERSION: i32 = 2;

/// 查询结果缓存字节预算（结果含整页释义 HTML，必须按体积计重）
const QUERY_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// 词条文档缓存字节预算
const DOC_CACHE_BYTES: usize = 32 * 1024 * 1024;

pub struct QueryCache {
    inner: MokaCache<String, Arc<Value>>,
    /// 词条文档 HTML 的短 TTL 缓存（图片版词典一份文档 35KB+，渲染含物化与
    /// mdd 资产探测；120s 过期，invalidate() 连带清空）
    docs: MokaCache<String, Arc<String>>,
}

/// JSON 值的内存粗估：每节点 16B 开销 + 字符串按字节 + String 头 24B。
/// 只用于缓存计重，量级对即可。（pub(crate) 供在线词典缓存复用）
pub(crate) fn json_weight(value: &Value) -> u64 {
    fn walk(value: &Value, acc: &mut u64) {
        match value {
            Value::String(s) => *acc += 24 + s.len() as u64 + 16,
            Value::Array(items) => {
                *acc += 16 + 8 * items.len() as u64;
                for item in items {
                    walk(item, acc);
                }
            }
            Value::Object(map) => {
                *acc += 16 + 24 * map.len() as u64;
                for (k, v) in map {
                    *acc += 24 + k.len() as u64;
                    walk(v, acc);
                }
            }
            _ => *acc += 24, // number/bool/null 定长
        }
    }
    let mut acc = 0u64;
    walk(value, &mut acc);
    acc
}

impl QueryCache {
    pub fn new() -> Self {
        Self {
            inner: MokaCache::builder()
                .max_capacity(QUERY_CACHE_BYTES as u64)
                .weigher(|_k, v: &Arc<Value>| json_weight(v).min(u32::MAX as u64) as u32)
                .time_to_live(Duration::from_secs(300))
                .build(),
            docs: MokaCache::builder()
                .max_capacity(DOC_CACHE_BYTES as u64)
                .weigher(|_k, v: &Arc<String>| v.len() as u32)
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

    /// 触发 moka 维护任务：真正释放过期条目占的内存（调度器周期调用）
    pub fn run_maintenance(&self) {
        self.inner.run_pending_tasks();
        self.docs.run_pending_tasks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_weight_scales_with_content() {
        let small = serde_json::json!({"a": 1});
        let apple = "apple".repeat(1000);
        let banana = "banana".repeat(1000);
        let big = serde_json::json!({
            "entries": [
                {"word": "苹果", "definition": apple},
                {"word": "香蕉", "definition": banana},
            ]
        });
        let ws = json_weight(&small);
        let wb = json_weight(&big);
        assert!(ws < 200, "小对象估算应很小：{ws}");
        assert!(wb > 2000, "大对象估算应随内容增长：{wb}");
    }
}

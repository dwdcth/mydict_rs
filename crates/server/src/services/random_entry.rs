//! 随机词条服务 —— 移植自 `app/services/random_entry_service.py`。
//!
//! M4 完整实现（主键区间加权随机 + 游标跳步）；本文件先提供词典删除/重解析时
//! 失效区间缓存的接口与数据结构，导入管线即可调用。

use std::sync::Mutex;
use std::time::Duration;

use moka::sync::Cache as MokaCache;

pub struct RandomBounds {
    /// (dictionary_id) -> (min_id, max_id)；TTL 1h
    inner: MokaCache<i32, (i64, i64)>,
    /// 预热单飞：避免多个触发点同时全量扫描
    pub warming: Mutex<()>,
}

impl RandomBounds {
    pub fn new() -> Self {
        Self {
            inner: MokaCache::builder()
                .max_capacity(1000)
                .time_to_live(Duration::from_secs(3600))
                .build(),
            warming: Mutex::new(()),
        }
    }

    pub fn get(&self, dictionary_id: i32) -> Option<(i64, i64)> {
        self.inner.get(&dictionary_id)
    }

    pub fn insert(&self, dictionary_id: i32, bounds: (i64, i64)) {
        self.inner.insert(dictionary_id, bounds);
    }

    pub fn invalidate(&self, dictionary_id: i32) {
        self.inner.invalidate(&dictionary_id);
    }

    /// 删除词典 / 重解析切换后整体失效
    pub fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }
}

// ── 随机词条 —— 移植自 `app/services/random_entry_service.py` ──────
//
// 主键区间加权随机 + 纯主键游标跳步（禁用 ORDER BY RANDOM()——全表扫描）；
// 归属与代际校验放应用侧（塞进 SQL 会让规划器放弃主键索引，搜韵实测 2.1s vs 毫秒级）。

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::AppState;

/// 游标跳步上限：超过视为这段区间密度过低，换下一部词典
const MAX_CURSOR_STEPS: usize = 50;

pub async fn warm_bounds_in_background(state: &std::sync::Arc<AppState>) {
    // 随机浏览开关关闭时一个查询都不发
    let enabled = crate::services::settings_service::get_bool_setting(
        &state.db,
        "random_browse_enabled",
        false,
    )
    .await
    .unwrap_or(false);
    if !enabled {
        return;
    }
    let state2 = state.clone();
    // 单飞：避免多个触发点同时全量扫描
    let guard = state2.random_bounds.warming.try_lock();
    if guard.is_err() {
        return;
    }
    std::mem::drop(guard);
    tokio::task::spawn(async move {
        if let Err(err) = warm_bounds(&state2).await {
            tracing::warn!(error = %err, "随机区间缓存预热失败");
        }
    });
}

async fn warm_bounds(state: &AppState) -> Result<(), AppError> {
    let dictionaries = crate::services::dictionary::list_dictionaries(&state.db).await?;
    for d in dictionaries {
        if d.status == "enabled" && state.random_bounds.get(d.id).is_none() {
            let _ = load_bounds(&state.db, d.id).await?;
        }
    }
    Ok(())
}

async fn load_bounds(
    db: &DatabaseConnection,
    dictionary_id: i32,
) -> Result<Option<(i64, i64)>, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            // 只看当前代：MIN/MAX 一次索引扫描
            "SELECT MIN(e.id) AS lo, MAX(e.id) AS hi FROM dict_entries e \
             JOIN dictionaries d ON e.dictionary_id = d.id AND e.generation = d.active_generation \
             WHERE e.dictionary_id = $1",
            [dictionary_id.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(None) };
    let lo: Option<i64> = row.try_get("", "lo").ok().flatten();
    let hi: Option<i64> = row.try_get("", "hi").ok().flatten();
    match (lo, hi) {
        (Some(lo), Some(hi)) if hi >= lo => Ok(Some((lo, hi))),
        _ => Ok(None),
    }
}

/// 挑一条随机词条：池 = enabled ∩ allowed ∩ dict_ids，按区间大小加权选词典，
/// 区间内随机落点后纯主键游标取第一条，归属/代际在应用侧校验。
/// 返回 (dictionary_id, dictionary_name, word, entry_id)
pub async fn pick_random_entry(
    state: &AppState,
    allowed_ids: Option<&[i32]>,
    dict_ids: Option<&[i32]>,
) -> Result<Option<Value>, AppError> {
    let mut pool: Vec<crate::entities::dictionary::Model> =
        crate::services::dictionary::list_dictionaries(&state.db)
            .await?
            .into_iter()
            .filter(|d| d.status == "enabled")
            .collect();
    if let Some(allowed) = allowed_ids {
        let set: std::collections::HashSet<i32> = allowed.iter().copied().collect();
        pool.retain(|d| set.contains(&d.id));
    }
    if let Some(ids) = dict_ids {
        let set: std::collections::HashSet<i32> = ids.iter().copied().collect();
        pool.retain(|d| set.contains(&d.id));
    }
    if pool.is_empty() {
        return Ok(None);
    }

    // 区间大小加权：先取各词典（缓存的）区间
    let mut weighted: Vec<(crate::entities::dictionary::Model, (i64, i64))> = Vec::new();
    for d in &pool {
        let bounds = if let Some(bounds) = state.random_bounds.get(d.id) {
            Some(bounds)
        } else {
            let bounds = load_bounds(&state.db, d.id).await?;
            if let Some(b) = bounds {
                state.random_bounds.insert(d.id, b);
            }
            bounds
        };
        if let Some(bounds) = bounds {
            weighted.push((d.clone(), bounds));
        }
    }
    if weighted.is_empty() {
        return Ok(None);
    }

    let total_weight: i64 = weighted.iter().map(|(_, (lo, hi))| hi - lo + 1).sum();
    // 加权随机选词典
    let mut pick = rand::random::<f64>() * total_weight as f64;
    let mut chosen = weighted.last().expect("non-empty");
    for item in &weighted {
        let w = item.1 .1 - item.1 .0 + 1;
        if pick < w as f64 {
            chosen = item;
            break;
        }
        pick -= w as f64;
    }
    let (dictionary, _) = chosen;

    // 区间内随机落点 + 纯主键游标（跳步上限 MAX_CURSOR_STEPS），失败顺位换下一部
    let mut index = weighted
        .iter()
        .position(|(d, _)| d.id == dictionary.id)
        .unwrap_or(0);
    for _attempt in 0..weighted.len() {
        let (dictionary, (lo, hi)) = &weighted[index];
        if let Some(entry) = random_entry_in_span(state, dictionary.id, *lo, *hi).await? {
            return Ok(Some(json!({
                "dictionary_id": dictionary.id,
                "dictionary_name": dictionary.name,
                "word": entry.0,
                "entry_id": entry.1,
            })));
        }
        index = (index + 1) % weighted.len();
    }
    Ok(None)
}

/// 区间随机落点后用 `id >= cursor ORDER BY id LIMIT 1` 取第一条，
/// dictionary_id/generation 在应用侧校验；跳步上限 MAX_CURSOR_STEPS。
async fn random_entry_in_span(
    state: &AppState,
    dictionary_id: i32,
    lo: i64,
    hi: i64,
) -> Result<Option<(String, i32)>, AppError> {
    use rand::Rng;
    let mut cursor: i64 = rand::rng().random_range(lo..=hi);
    for _ in 0..MAX_CURSOR_STEPS {
        let row = state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                state.db.get_database_backend(),
                "SELECT e.id, e.word, e.dictionary_id, e.generation, d.active_generation \
                 FROM dict_entries e JOIN dictionaries d ON d.id = e.dictionary_id \
                 WHERE e.id >= $1 ORDER BY e.id LIMIT 1",
                [cursor.into()],
            ))
            .await?;
        let Some(row) = row else { return Ok(None) };
        let id: i64 = row.try_get("", "id").unwrap_or_default();
        let row_dict: i32 = row.try_get("", "dictionary_id").unwrap_or_default();
        let generation: i32 = row.try_get("", "generation").unwrap_or_default();
        let active: i32 = row.try_get("", "active_generation").unwrap_or_default();
        let word: String = row.try_get("", "word").unwrap_or_default();
        // 落点已越过这部词典的区间
        if id > hi {
            return Ok(None);
        }
        if row_dict == dictionary_id && generation == active {
            return Ok(Some((word, id as i32)));
        }
        cursor = id + 1;
    }
    Ok(None)
}

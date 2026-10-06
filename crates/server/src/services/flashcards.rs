//! 间隔重复复习（FSRS，rs-fsrs 调度器）—— 闪卡挂在生词本条目上。
//!
//! 模型：`flashcards.vocab_item_id` 即卡片主键；调度状态列与 rs-fsrs 的 `Card`
//! 字段一一对应（unix 秒 ↔ `DateTime<Utc>`、state 0-3 ↔ `rs_fsrs::State`）。
//! 参数每用户可调：`users.fsrs_retention`（目标记忆率）与 `fsrs_weights`
//! （19 位 FSRS-4.5 权重 JSON；NULL = 默认）。

use chrono::{DateTime, Utc};
use fsrs::{FSRS, DEFAULT_PARAMETERS, MemoryState};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::AppState;

/// fsrs crate 接受的权重长度（FSRS-4.5/5/6；短的会自动补齐升级到 21 位 FSRS-6）
const ACCEPTED_WEIGHT_COUNTS: [usize; 3] = [17, 19, 21];
/// 目标记忆率合理区间
const RETENTION_RANGE: (f64, f64) = (0.7, 0.99);
/// 单次复习会话默认取的队列长度
pub const QUEUE_LIMIT: i64 = 50;

/// 用户的 FSRS 配置（模型 + 目标记忆率；retention 按 fsrs 的 per-call 语义传）
struct UserFsrs {
    model: FSRS,
    retention: f32,
}

fn datetime_to_ts(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

/// 用户的 FSRS 模型：默认 FSRS-6（21 参数，Anki 当前默认）；自定义权重按长度
/// 自动识别版本（17/19/21/34，短版本会被 check_and_fill 自动补齐升级）
pub fn build_model(weights: Option<&str>) -> Result<FSRS, AppError> {
    let params: Vec<f32> = match weights {
        Some(raw) => match serde_json::from_str::<Vec<f64>>(raw) {
            Ok(list)
                if ACCEPTED_WEIGHT_COUNTS.contains(&list.len())
                    && list.iter().all(|w| w.is_finite()) =>
            {
                list.iter().map(|v| *v as f32).collect()
            }
            _ => {
                return Err(AppError::validation(format!(
                    "权重须是 {} 中任一长度的数值 JSON 数组（FSRS-4.5/5/6/7）",
                    ACCEPTED_WEIGHT_COUNTS
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                )))
            }
        },
        None => DEFAULT_PARAMETERS.to_vec(),
    };
    FSRS::new(&params).map_err(|e| AppError::validation(format!("权重无效：{e}")))
}

async fn user_fsrs(db: &DatabaseConnection, user_id: i32) -> Result<UserFsrs, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT fsrs_retention, fsrs_weights FROM users WHERE id = $1",
            [user_id.into()],
        ))
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("用户不存在"))?;
    let retention = row
        .try_get::<Option<f64>>("", "fsrs_retention")
        .ok()
        .flatten()
        .unwrap_or(0.9)
        .clamp(RETENTION_RANGE.0, RETENTION_RANGE.1) as f32;
    // 权重列由写入路径保证合法；万一非法（手改库）回退默认而不是 500
    let weights = row
        .try_get::<Option<String>>("", "fsrs_weights")
        .ok()
        .flatten();
    let model = build_model(weights.as_deref()).unwrap_or_else(|_| {
        FSRS::new(&DEFAULT_PARAMETERS).expect("默认参数合法")
    });
    Ok(UserFsrs { model, retention })
}

/// 数据库行 → MemoryState（reps=0 的新卡传 None）
fn memory_of(row: &sea_orm::QueryResult) -> Option<MemoryState> {
    let reps = row.try_get::<i32>("", "reps").unwrap_or_default();
    if reps == 0 {
        return None;
    }
    Some(MemoryState {
        stability: row.try_get::<f64>("", "stability").unwrap_or_default() as f32,
        difficulty: row.try_get::<f64>("", "difficulty").unwrap_or_default() as f32,
    })
}

/// 距上次复习的天数（新卡 0）
fn elapsed_days_of(row: &sea_orm::QueryResult) -> u32 {
    let reps = row.try_get::<i32>("", "reps").unwrap_or_default();
    if reps == 0 {
        return 0;
    }
    let last = row.try_get::<i64>("", "last_review").unwrap_or_default();
    (Utc::now().timestamp() - last).max(0).div_euclid(86400) as u32
}

/// fsrs 的 f32 天间隔 → 我们的天粒度整数（Again 也落明天，与原 longterm 语义一致）
fn interval_days(interval: f32) -> i64 {
    interval.round().max(1.0) as i64
}

fn human_interval(days: i64) -> String {
    // 与 Anki 一致的观感：<1 天显示「今天」，其余显示天数
    if days <= 0 {
        "今天".to_string()
    } else {
        format!("{days} 天")
    }
}

/// 加入闪卡：词条不存在则先进生词本（同快照管线），再挂调度行（幂等）
pub async fn add_card(
    state: &AppState,
    user_id: i32,
    dictionary_id: Option<i32>,
    word: &str,
) -> Result<Value, AppError> {
    let word = word.trim();
    if word.is_empty() {
        return Err(AppError::validation("单词不能为空"));
    }
    // 已在生词本 → 直接复用；不在 → 走标准收藏（含 @@@LINK 快照/上限/去重口径）
    let existing = find_vocab_item(state, user_id, dictionary_id, word).await?;
    let item_id = match existing {
        Some(id) => id,
        None => {
            let created = crate::services::vocab_service::add_vocab_item(
                state,
                crate::services::vocab_service::OwnerKind::User,
                user_id,
                word,
                dictionary_id,
                None,
            )
            .await?;
            created["id"].as_i64().unwrap_or_default() as i32
        }
    };

    let now = Utc::now();
    let inserted = state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "INSERT OR IGNORE INTO flashcards (vocab_item_id, due, created_at) VALUES ($1, $2, $3)",
            [item_id.into(), datetime_to_ts(now).into(), datetime_to_ts(now).into()],
        ))
        .await
        .map_err(AppError::from)?;
    Ok(json!({
        "vocab_item_id": item_id,
        "already": inserted.rows_affected() == 0,
        "due_at": datetime_to_ts(now),
    }))
}

/// 按生词本同口径找已有条目（word + dictionary_id 精确）
async fn find_vocab_item(
    state: &AppState,
    user_id: i32,
    dictionary_id: Option<i32>,
    word: &str,
) -> Result<Option<i32>, AppError> {
    let backend = state.db.get_database_backend();
    let sql = match dictionary_id {
        Some(_) => "SELECT id FROM vocab_items WHERE user_id = $1 AND word = $2 AND dictionary_id = $3 LIMIT 1",
        None => "SELECT id FROM vocab_items WHERE user_id = $1 AND word = $2 AND dictionary_id IS NULL LIMIT 1",
    };
    let mut values = vec![user_id.into(), word.to_string().into()];
    if let Some(id) = dictionary_id {
        values.push(id.into());
    }
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
        .map_err(AppError::from)?;
    Ok(row.and_then(|r| r.try_get::<i32>("", "id").ok()))
}

/// 复习队列：到期卡（due ≤ 现在，按 due 升序）在前，新卡按加入顺序补足。
/// 每张附四档预测间隔（评分按钮直接展示）。
pub async fn review_queue(
    state: &AppState,
    user_id: i32,
    limit: i64,
) -> Result<Value, AppError> {
    let fsrs = user_fsrs(&state.db, user_id).await?;
    let now = Utc::now();
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "SELECT f.*, v.word, v.phonetic, v.dictionary_id, d.name AS dictionary_name \
             FROM flashcards f \
             JOIN vocab_items v ON v.id = f.vocab_item_id \
             LEFT JOIN dictionaries d ON d.id = v.dictionary_id \
             WHERE v.user_id = $1 AND f.due <= $2 \
             ORDER BY (f.state = 0), f.due, f.vocab_item_id \
             LIMIT $3",
            [user_id.into(), datetime_to_ts(now).into(), limit.into()],
        ))
        .await
        .map_err(AppError::from)?;

    let items: Vec<Value> = rows
        .iter()
        .map(|row| -> Result<Value, AppError> {
            // 四档预测（只算不写库）
            let states = fsrs
                .model
                .next_states(memory_of(row), fsrs.retention, elapsed_days_of(row))
                .map_err(|e| AppError::internal("fsrs", e))?;
            let preview = |item: &fsrs::ItemState| -> Value {
                let days = interval_days(item.interval);
                json!({
                    "interval_days": days,
                    "label": human_interval(days),
                })
            };
            Ok(json!({

                "vocab_item_id": row.try_get::<i32>("", "vocab_item_id").unwrap_or_default(),
                "word": row.try_get::<String>("", "word").unwrap_or_default(),
                "phonetic": row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                "dictionary_id": row.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
                "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
                "state": row.try_get::<i64>("", "state").unwrap_or_default(),
                "reps": row.try_get::<i32>("", "reps").unwrap_or_default(),
                "lapses": row.try_get::<i32>("", "lapses").unwrap_or_default(),
                "previews": {
                    "again": preview(&states.again),
                    "hard": preview(&states.hard),
                    "good": preview(&states.good),
                    "easy": preview(&states.easy),
                },
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(json!({ "queue": items, "now": datetime_to_ts(now) }))
}

/// 评一次分：FSRS 调度 → 更新卡 + 写日志
pub async fn review_card(
    state: &AppState,
    user_id: i32,
    vocab_item_id: i32,
    rating: i32,
) -> Result<Value, AppError> {
    if !(1..=4).contains(&rating) {
        return Err(AppError::validation("rating 须为 1-4"));
    }
    let backend = state.db.get_database_backend();
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT f.* FROM flashcards f JOIN vocab_items v ON v.id = f.vocab_item_id \
             WHERE f.vocab_item_id = $1 AND v.user_id = $2",
            [vocab_item_id.into(), user_id.into()],
        ))
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("闪卡不存在"))?;

    let fsrs = user_fsrs(&state.db, user_id).await?;
    let now = Utc::now();
    let elapsed = elapsed_days_of(&row);
    let states = fsrs
        .model
        .next_states(memory_of(&row), fsrs.retention, elapsed)
        .map_err(|e| AppError::internal("fsrs", e))?;
    let (item, chosen) = match rating {
        1 => (&states.again, "again"),
        2 => (&states.hard, "hard"),
        3 => (&states.good, "good"),
        _ => (&states.easy, "easy"),
    };
    let _ = chosen;
    let memory = item.memory;
    let scheduled = interval_days(item.interval);
    let previous_reps = row.try_get::<i32>("", "reps").unwrap_or_default();
    let previous_lapses = row.try_get::<i32>("", "lapses").unwrap_or_default();
    // 状态迁移（显示用）：Again → 重学中；其余 → 复习中
    let new_state = if rating == 1 { 3 } else { 2 };
    let new_reps = previous_reps + 1;
    let new_lapses = previous_lapses + i32::from(rating == 1);
    let due = datetime_to_ts(now + chrono::Duration::days(scheduled));

    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE flashcards SET state=$1, due=$2, stability=$3, difficulty=$4, \
             elapsed_days=$5, scheduled_days=$6, reps=$7, lapses=$8, last_review=$9, \
             stability_fast=$10, last_rating=$11 \
             WHERE vocab_item_id = $12",
            [
                new_state.into(),
                due.into(),
                (memory.stability as f64).into(),
                (memory.difficulty as f64).into(),
                (elapsed as i64).into(),
                scheduled.into(),
                new_reps.into(),
                new_lapses.into(),
                datetime_to_ts(now).into(),
                (memory.stability as f64).into(),
                rating.into(),
                vocab_item_id.into(),
            ],
        ))
        .await
        .map_err(AppError::from)?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO flashcard_reviews (flashcard_id, rating, state, elapsed_days, scheduled_days, reviewed_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            [
                vocab_item_id.into(),
                rating.into(),
                new_state.into(),
                (elapsed as i64).into(),
                scheduled.into(),
                datetime_to_ts(now).into(),
            ],
        ))
        .await
        .map_err(AppError::from)?;

    Ok(json!({
        "vocab_item_id": vocab_item_id,
        "interval_days": scheduled,
        "interval_label": human_interval(scheduled),
        "due_at": due,
        "state": new_state,
        "reps": new_reps,
        "lapses": new_lapses,
    }))
}

/// 闪卡列表（管理用）：due=只看到期+新卡 / all=全部
pub async fn list_cards(
    state: &AppState,
    user_id: i32,
    filter: &str,
    page: i64,
    page_size: i64,
) -> Result<Value, AppError> {
    let backend = state.db.get_database_backend();
    let now = datetime_to_ts(Utc::now());
    let (cond, params) = match filter {
        "due" => ("AND f.due <= $3", vec![now.into()]),
        _ => ("", Vec::new()),
    };
    let mut values = vec![user_id.into(), page_size.into(), ((page - 1).max(0) * page_size).into()];
    values.extend(params);
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT f.*, v.word, v.phonetic, d.name AS dictionary_name \
                 FROM flashcards f \
                 JOIN vocab_items v ON v.id = f.vocab_item_id \
                 LEFT JOIN dictionaries d ON d.id = v.dictionary_id \
                 WHERE v.user_id = $1 {cond} \
                 ORDER BY f.due, f.vocab_item_id LIMIT $2 OFFSET $4"
            ),
            values,
        ))
        .await
        .map_err(AppError::from)?;
    let stats = stats(state, user_id).await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "vocab_item_id": row.try_get::<i32>("", "vocab_item_id").unwrap_or_default(),
                "word": row.try_get::<String>("", "word").unwrap_or_default(),
                "phonetic": row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
                "state": row.try_get::<i64>("", "state").unwrap_or_default(),
                "due_at": row.try_get::<i64>("", "due").unwrap_or_default(),
                "reps": row.try_get::<i32>("", "reps").unwrap_or_default(),
                "lapses": row.try_get::<i32>("", "lapses").unwrap_or_default(),
            })
        })
        .collect();
    let mut out = json!({ "items": items });
    if let (Some(obj), Some(target)) = (stats.as_object(), out.as_object_mut()) {
        for (key, value) in obj {
            target.insert(key.clone(), value.clone());
        }
    }
    Ok(out)
}

/// 徽章/进度统计：到期数、新卡数、总数、今日已复习
pub async fn stats(state: &AppState, user_id: i32) -> Result<Value, AppError> {
    let backend = state.db.get_database_backend();
    let now = datetime_to_ts(Utc::now());
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS total, \
                SUM(CASE WHEN due <= $2 THEN 1 ELSE 0 END) AS due_count, \
                SUM(CASE WHEN state = 0 THEN 1 ELSE 0 END) AS new_count \
             FROM flashcards f JOIN vocab_items v ON v.id = f.vocab_item_id \
             WHERE v.user_id = $1",
            [user_id.into(), now.into()],
        ))
        .await
        .map_err(AppError::from)?
        .expect("count row");
    // 今日已复习（本地日界，口径同限流/统计）
    let zone = crate::core::timeutil::local_zone(&state.cfg.timezone);
    let (start, end) = crate::core::timeutil::day_bounds_unix(
        zone,
        crate::core::timeutil::now_local_date(zone),
    )
    .unwrap_or((0, i64::MAX));
    let today = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT COUNT(*) AS n FROM flashcard_reviews r \
             JOIN vocab_items v ON v.id = r.flashcard_id \
             WHERE v.user_id = $1 AND r.reviewed_at >= $2 AND r.reviewed_at < $3",
            [user_id.into(), start.into(), end.into()],
        ))
        .await
        .map_err(AppError::from)?
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    Ok(json!({
        "total": row.try_get::<i64>("", "total").unwrap_or(0),
        "due_count": row.try_get::<i64>("", "due_count").unwrap_or(0),
        "new_count": row.try_get::<i64>("", "new_count").unwrap_or(0),
        "today_reviewed": today,
    }))
}

/// 移出复习（生词本条目保留）
pub async fn delete_card(
    state: &AppState,
    user_id: i32,
    vocab_item_id: i32,
) -> Result<(), AppError> {
    let deleted = state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            "DELETE FROM flashcards WHERE vocab_item_id = $1 \
             AND vocab_item_id IN (SELECT id FROM vocab_items WHERE user_id = $2)",
            [vocab_item_id.into(), user_id.into()],
        ))
        .await
        .map_err(AppError::from)?;
    if deleted.rows_affected() == 0 {
        return Err(AppError::not_found("闪卡不存在"));
    }
    Ok(())
}

/// 复习参数：retention（0.7-0.99）与自定义权重（19 位 JSON 数组，空串=清空回默认）
pub async fn get_settings(db: &DatabaseConnection, user_id: i32) -> Result<Value, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT fsrs_retention, fsrs_weights FROM users WHERE id = $1",
            [user_id.into()],
        ))
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("用户不存在"))?;
    Ok(json!({
        "retention": row.try_get::<Option<f64>>("", "fsrs_retention").ok().flatten(),
        "weights": row.try_get::<Option<String>>("", "fsrs_weights").ok().flatten(),
    }))
}

pub async fn update_settings(
    db: &DatabaseConnection,
    user_id: i32,
    retention: Option<f64>,
    weights: Option<&str>,
) -> Result<Value, AppError> {
    if let Some(r) = retention {
        if !(RETENTION_RANGE.0..=RETENTION_RANGE.1).contains(&r) {
            return Err(AppError::validation(format!(
                "目标记忆率须在 {}-{} 之间",
                RETENTION_RANGE.0, RETENTION_RANGE.1
            )));
        }
    }
    // weights：None=不动；Some("")=清空回默认；Some(json)=19 位数值数组
    let weights_db: Option<Option<String>> = match weights {
        None => None,
        Some("") => Some(None),
        Some(raw) => match serde_json::from_str::<Vec<f64>>(raw) {
            Ok(list)
                if ACCEPTED_WEIGHT_COUNTS.contains(&list.len())
                    && list.iter().all(|w| w.is_finite()) =>
            {
                // 再让 fsrs crate 自己过一遍（check_and_fill 会补齐/校验）
                build_model(Some(raw))?;
                Some(Some(raw.to_string()))
            }
            _ => {
                return Err(AppError::validation(format!(
                    "权重须是 {} 中任一长度的数值 JSON 数组（FSRS-4.5/5/6/7，短版本自动补齐）",
                    ACCEPTED_WEIGHT_COUNTS
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                )))
            }
        },
    };
    let backend = db.get_database_backend();
    if let Some(r) = retention {
        db.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET fsrs_retention = $1 WHERE id = $2",
            [(Some(r)).into(), user_id.into()],
        ))
        .await
        .map_err(AppError::from)?;
    }
    if let Some(w) = weights_db {
        db.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE users SET fsrs_weights = $1 WHERE id = $2",
            [w.into(), user_id.into()],
        ))
        .await
        .map_err(AppError::from)?;
    }
    get_settings(db, user_id).await
}

//! 间隔重复复习（FSRS，rs-fsrs 调度器）—— 闪卡挂在生词本条目上。
//!
//! 模型：`flashcards.vocab_item_id` 即卡片主键；调度状态列与 rs-fsrs 的 `Card`
//! 字段一一对应（unix 秒 ↔ `DateTime<Utc>`、state 0-3 ↔ `rs_fsrs::State`）。
//! 参数每用户可调：`users.fsrs_retention`（目标记忆率）与 `fsrs_weights`
//! （19 位 FSRS-4.5 权重 JSON；NULL = 默认）。

use chrono::{DateTime, Utc};
use rs_fsrs::{Card, FSRS, Parameters, Rating};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::AppState;

/// 权重维度（FSRS-4.5）
const WEIGHT_COUNT: usize = 19;
/// 目标记忆率合理区间
const RETENTION_RANGE: (f64, f64) = (0.7, 0.99);
/// 单次复习会话默认取的队列长度
pub const QUEUE_LIMIT: i64 = 50;

fn ts_to_datetime(ts: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(ts, 0).unwrap_or_else(Utc::now)
}

fn datetime_to_ts(dt: DateTime<Utc>) -> i64 {
    dt.timestamp()
}

/// 用户的 FSRS 实例：目标记忆率/自定义权重（缺失或非法则回默认）
pub fn fsrs_for(retention: Option<f64>, weights: Option<&str>) -> FSRS {
    let mut params = Parameters {
        // 天粒度调度（无当日分钟级学习步骤）：词典复习是低频场景，
        // Again 的复习落在明天而不是几分钟后
        enable_short_term: false,
        ..Parameters::default()
    };
    if let Some(r) = retention {
        params.request_retention = r.clamp(RETENTION_RANGE.0, RETENTION_RANGE.1);
    }
    if let Some(raw) = weights {
        if let Ok(list) = serde_json::from_str::<Vec<f64>>(raw) {
            if list.len() == WEIGHT_COUNT && list.iter().all(|w| w.is_finite()) {
                let mut w = params.w;
                for (i, v) in list.iter().enumerate() {
                    w[i] = *v;
                }
                params.w = w;
            }
        }
    }
    FSRS::new(params)
}

async fn user_fsrs(db: &DatabaseConnection, user_id: i32) -> Result<FSRS, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT fsrs_retention, fsrs_weights FROM users WHERE id = $1",
            [user_id.into()],
        ))
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("用户不存在"))?;
    Ok(fsrs_for(
        row.try_get::<Option<f64>>("", "fsrs_retention")
            .ok()
            .flatten(),
        row.try_get::<Option<String>>("", "fsrs_weights")
            .ok()
            .flatten()
            .as_deref(),
    ))
}

/// 数据库行 → rs-fsrs Card
fn card_from(row: &sea_orm::QueryResult) -> Card {
    Card {
        due: ts_to_datetime(row.try_get::<i64>("", "due").unwrap_or_default()),
        stability: row.try_get::<f64>("", "stability").unwrap_or_default(),
        difficulty: row.try_get::<f64>("", "difficulty").unwrap_or_default(),
        elapsed_days: row.try_get::<i64>("", "elapsed_days").unwrap_or_default(),
        scheduled_days: row.try_get::<i64>("", "scheduled_days").unwrap_or_default(),
        reps: row.try_get::<i32>("", "reps").unwrap_or_default(),
        lapses: row.try_get::<i32>("", "lapses").unwrap_or_default(),
        state: match row.try_get::<i64>("", "state").unwrap_or_default() {
            1 => rs_fsrs::State::Learning,
            2 => rs_fsrs::State::Review,
            3 => rs_fsrs::State::Relearning,
            _ => rs_fsrs::State::New,
        },
        last_review: ts_to_datetime(row.try_get::<i64>("", "last_review").unwrap_or_default()),
    }
}

fn state_to_i32(state: rs_fsrs::State) -> i32 {
    match state {
        rs_fsrs::State::New => 0,
        rs_fsrs::State::Learning => 1,
        rs_fsrs::State::Review => 2,
        rs_fsrs::State::Relearning => 3,
    }
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
        .map(|row| {
            let card = card_from(row);
            // 四档预测（repeat 不写库，只算）
            let log = fsrs.repeat(card.clone(), now);
            let preview = |rating: Rating| -> Value {
                match log.get(&rating) {
                    Some(info) => json!({
                        "interval_days": info.card.scheduled_days,
                        "label": human_interval(info.card.scheduled_days),
                    }),
                    None => Value::Null,
                }
            };
            json!({
                "vocab_item_id": row.try_get::<i32>("", "vocab_item_id").unwrap_or_default(),
                "word": row.try_get::<String>("", "word").unwrap_or_default(),
                "phonetic": row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
                "dictionary_id": row.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
                "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
                "state": row.try_get::<i64>("", "state").unwrap_or_default(),
                "reps": row.try_get::<i32>("", "reps").unwrap_or_default(),
                "lapses": row.try_get::<i32>("", "lapses").unwrap_or_default(),
                "previews": {
                    "again": preview(Rating::Again),
                    "hard": preview(Rating::Hard),
                    "good": preview(Rating::Good),
                    "easy": preview(Rating::Easy),
                },
            })
        })
        .collect();
    Ok(json!({ "queue": items, "now": datetime_to_ts(now) }))
}

/// 评一次分：FSRS 调度 → 更新卡 + 写日志
pub async fn review_card(
    state: &AppState,
    user_id: i32,
    vocab_item_id: i32,
    rating: i32,
) -> Result<Value, AppError> {
    let rating = match rating {
        1 => Rating::Again,
        2 => Rating::Hard,
        3 => Rating::Good,
        4 => Rating::Easy,
        _ => return Err(AppError::validation("rating 须为 1-4")),
    };
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
    let info = fsrs.next(card_from(&row), now, rating);
    let next = info.card;

    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE flashcards SET state=$1, due=$2, stability=$3, difficulty=$4, \
             elapsed_days=$5, scheduled_days=$6, reps=$7, lapses=$8, last_review=$9 \
             WHERE vocab_item_id = $10",
            [
                state_to_i32(next.state).into(),
                datetime_to_ts(next.due).into(),
                next.stability.into(),
                next.difficulty.into(),
                next.elapsed_days.into(),
                next.scheduled_days.into(),
                next.reps.into(),
                next.lapses.into(),
                datetime_to_ts(next.last_review).into(),
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
                (rating as i32).into(),
                state_to_i32(info.review_log.state).into(),
                info.review_log.elapsed_days.into(),
                info.review_log.scheduled_days.into(),
                datetime_to_ts(now).into(),
            ],
        ))
        .await
        .map_err(AppError::from)?;

    Ok(json!({
        "vocab_item_id": vocab_item_id,
        "interval_days": next.scheduled_days,
        "interval_label": human_interval(next.scheduled_days),
        "due_at": datetime_to_ts(next.due),
        "state": state_to_i32(next.state),
        "reps": next.reps,
        "lapses": next.lapses,
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
            Ok(list) if list.len() == WEIGHT_COUNT && list.iter().all(|w| w.is_finite()) => {
                Some(Some(raw.to_string()))
            }
            _ => {
                return Err(AppError::validation(format!(
                    "权重须是 {WEIGHT_COUNT} 个数值的 JSON 数组（FSRS-4.5 格式）"
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

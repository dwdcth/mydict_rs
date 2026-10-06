//! 生词本 —— 移植自 `app/services/vocab_service.py`。
//! 两套表（user/token）同构，按 OwnerKind 分流；收藏存释义快照（@@@LINK 已解引用）。

use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};

use crate::core::errors::AppError;
use crate::core::timeutil::{unix_to_iso};
use crate::services::query as query_service;
use crate::services::settings_service;
use crate::AppState;

#[derive(Clone, Copy, PartialEq)]
pub enum OwnerKind {
    Token,
    User,
}

impl OwnerKind {
    fn table(self) -> &'static str {
        match self {
            OwnerKind::Token => "token_vocab_items",
            OwnerKind::User => "vocab_items",
        }
    }
    fn owner_col(self) -> &'static str {
        match self {
            OwnerKind::Token => "token_id",
            OwnerKind::User => "user_id",
        }
    }
}

async fn max_items_per_owner(state: &AppState) -> Result<Option<i64>, AppError> {
    let raw = settings_service::get_setting(&state.db, "vocab_max_items_per_owner", None).await?;
    match raw {
        Some(raw) if !raw.trim().is_empty() => Ok(raw.trim().parse().ok()),
        _ => Ok(None),
    }
}

/// 找词条：指定词典则精确查；否则按语言路由顺序取第一个命中
async fn find_entry(
    state: &AppState,
    word: &str,
    dictionary_id: Option<i32>,
) -> Result<(query_service::EntryRow, Option<i32>), AppError> {
    let word_lower = word.trim().to_lowercase();
    // 裸词条（不解 @@@LINK）：词头要存用户查到的那个，快照才与查询结果口径一致
    if let Some(dictionary_id) = dictionary_id {
        let entry = query_service::get_raw_entry(&state.db, dictionary_id, &word_lower).await?;
        return match entry {
            Some(entry) => Ok((entry, Some(dictionary_id))),
            None => Err(AppError::not_found("该词典下未找到该单词，无法收藏")),
        };
    }
    let dictionaries = query_service::resolve_dictionaries(
        &state.db, word, None, None, None, None,
    )
    .await?;
    for dict in dictionaries {
        if let Some(entry) = query_service::get_raw_entry(&state.db, dict.id, &word_lower).await? {
            return Ok((entry, Some(dict.id)));
        }
    }
    Err(AppError::not_found("未找到该单词的释义，无法收藏"))
}

pub async fn add_vocab_item(
    state: &AppState,
    kind: OwnerKind,
    owner_id: i32,
    word: &str,
    dictionary_id: Option<i32>,
    note: Option<&str>,
) -> Result<Value, AppError> {
    let (entry, resolved_dict_id) = find_entry(state, word, dictionary_id).await?;
    // 快照存真正承载内容的条目（@@@LINK 已解）；词头沿用用户查到的那个
    let content = query_service::resolve_entry_link(state, &entry).await?;

    // 词条级去重（dictionary_id 为 NULL 时唯一索引管不到，这里兜住）
    let backend = state.db.get_database_backend();
    let dict_condition = match resolved_dict_id {
        Some(id) => format!("AND dictionary_id = {id}"),
        None => "AND dictionary_id IS NULL".to_string(),
    };
    let duplicate = state
        .db
        .query_one_raw(Statement::from_string(
            backend,
            format!(
                "SELECT id FROM {} WHERE {} = {} AND word = '{}' {} LIMIT 1",
                kind.table(),
                kind.owner_col(),
                owner_id,
                entry.word.replace('\'', "''"),
                dict_condition
            ),
        ))
        .await?
        .is_some();
    if duplicate {
        return Err(AppError::conflict("该词典下已收藏该单词"));
    }

    if let Some(max) = max_items_per_owner(state).await? {
        let count = state
            .db
            .query_one_raw(Statement::from_string(
                backend,
                format!(
                    "SELECT COUNT(*) AS n FROM {} WHERE {} = {}",
                    kind.table(),
                    kind.owner_col(),
                    owner_id
                ),
            ))
            .await?
            .and_then(|r| r.try_get::<i64>("", "n").ok())
            .unwrap_or(0);
        if count >= max {
            return Err(AppError::conflict(format!("生词本已达上限（{max} 条）")));
        }
    }

    // 词典名快照：词典日后被删（dictionary_id 置 NULL）列表仍要显示来源
    let dictionary_name: Option<String> = match resolved_dict_id {
        Some(id) => state
            .db
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT name FROM dictionaries WHERE id = $1",
                [id.into()],
            ))
            .await?
            .and_then(|r| r.try_get::<String>("", "name").ok()),
        None => None,
    };

    let now = chrono::Utc::now().timestamp();
    let result = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            &format!(
                "INSERT INTO {} ({}, word, dictionary_id, dictionary_name, phonetic, definition, note, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
                kind.table(),
                kind.owner_col()
            ),
            [
                owner_id.into(),
                entry.word.clone().into(),
                resolved_dict_id.into(),
                dictionary_name.into(),
                (content.phonetic.clone().or_else(|| entry.phonetic.clone())).into(),
                content.definition.clone().into(),
                note.map(String::from).into(),
                now.into(),
            ],
        ))
        .await?;
    let item_id: i32 = result
        .and_then(|r| r.try_get("", "id").ok())
        .unwrap_or_default();
    get_vocab_item_json(state, kind, item_id).await
}

async fn get_vocab_item_json(state: &AppState, kind: OwnerKind, item_id: i32) -> Result<Value, AppError> {
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            &format!("SELECT * FROM {} WHERE id = $1", kind.table()),
            [item_id.into()],
        ))
        .await?;
    let Some(row) = row else {
        return Err(AppError::not_found("生词不存在"));
    };
    Ok(vocab_row_to_json(&row))
}

fn vocab_row_to_json(row: &sea_orm::QueryResult) -> Value {
    json!({
        "id": row.try_get::<i32>("", "id").unwrap_or_default(),
        "word": row.try_get::<String>("", "word").unwrap_or_default(),
        "phonetic": row.try_get::<Option<String>>("", "phonetic").ok().flatten(),
        "definition": row.try_get::<Option<String>>("", "definition").ok().flatten(),
        "note": row.try_get::<Option<String>>("", "note").ok().flatten(),
        "dictionary_id": row.try_get::<Option<i32>>("", "dictionary_id").ok().flatten(),
        "dictionary_name": row.try_get::<Option<String>>("", "dictionary_name").ok().flatten(),
        "created_at": unix_to_iso(row.try_get::<i64>("", "created_at").unwrap_or_default()),
        "in_review": row.try_get::<Option<i32>>("", "in_review").ok().flatten().map(|v| v != 0).unwrap_or(false),
    })
}

pub struct VocabListParams {
    pub search: Option<String>,
    pub page: i64,
    pub page_size: i64,
    pub sort_by: &'static str, // word | date
    pub order: &'static str,   // asc | desc
    pub lang_from: Option<String>,
}

pub async fn list_vocab_items(
    state: &AppState,
    kind: OwnerKind,
    owner_id: i32,
    params: &VocabListParams,
) -> Result<(Vec<Value>, i64), AppError> {
    let backend = state.db.get_database_backend();
    // 用户可控的 search/lang 一律参数化（Python 走 SQLAlchemy 参数绑定）
    let mut where_sql = format!("v.{} = {}", kind.owner_col(), owner_id);
    let mut values: Vec<sea_orm::Value> = Vec::new();
    let mut ph = crate::services::query::PhPub::new(backend);
    if let Some(search) = params.search.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let p = ph.take();
        where_sql.push_str(&format!(" AND v.word LIKE '%' || {p} || '%'"));
        values.push(search.into());
    }
    if let Some(lang) = params.lang_from.as_deref().filter(|s| !s.is_empty()) {
        // JOIN dictionaries 过滤来源语言
        let p = ph.take();
        where_sql.push_str(&format!(
            " AND v.dictionary_id IN (SELECT id FROM dictionaries WHERE lang_from = {p})"
        ));
        values.push(lang.to_string().into());
    }
    let total: i64 = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT COUNT(*) AS n FROM {} v WHERE {where_sql}",
                kind.table()
            ),
            values.clone(),
        ))
        .await?
        .and_then(|r| r.try_get("", "n").ok())
        .unwrap_or(0);
    let sort_col = match params.sort_by {
        "word" => "v.word",
        _ => "v.created_at",
    };
    let dir = if params.order == "asc" { "ASC" } else { "DESC" };
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT v.*, EXISTS(SELECT 1 FROM flashcards f WHERE f.vocab_item_id = v.id) AS in_review \
                 FROM {} v WHERE {where_sql} ORDER BY {sort_col} {dir}, v.id {dir} LIMIT {} OFFSET {}",
                kind.table(),
                params.page_size,
                (params.page - 1).max(0) * params.page_size
            ),
            values,
        ))
        .await?;
    Ok((rows.iter().map(vocab_row_to_json).collect(), total))
}

/// 生词本来源词典的语言列表（去重排序，语言 tab 用）
pub async fn list_owner_languages(
    state: &AppState,
    kind: OwnerKind,
    owner_id: i32,
) -> Result<Vec<String>, AppError> {
    let rows = state
        .db
        .query_all_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            &format!(
                "SELECT DISTINCT d.lang_from FROM {} v \
                 JOIN dictionaries d ON d.id = v.dictionary_id \
                 WHERE v.{} = $1 ORDER BY d.lang_from",
                kind.table(),
                kind.owner_col()
            ),
            [owner_id.into()],
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "lang_from").ok())
        .collect())
}

/// 按 id 取一条（校验归属），供渲染/删除
pub async fn get_vocab_item(
    state: &AppState,
    kind: OwnerKind,
    owner_id: i32,
    item_id: i32,
) -> Result<Value, AppError> {
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            &format!(
                "SELECT * FROM {} WHERE id = $1 AND {} = $2",
                kind.table(),
                kind.owner_col()
            ),
            [item_id.into(), owner_id.into()],
        ))
        .await?;
    row.map(|r| vocab_row_to_json(&r))
        .ok_or_else(|| AppError::not_found("生词不存在"))
}

pub async fn delete_vocab_item(
    state: &AppState,
    kind: OwnerKind,
    owner_id: i32,
    item_id: i32,
) -> Result<(), AppError> {
    get_vocab_item(state, kind, owner_id, item_id).await?;
    state
        .db
        .execute_raw(Statement::from_sql_and_values(
            state.db.get_database_backend(),
            &format!("DELETE FROM {} WHERE id = $1", kind.table()),
            [item_id.into()],
        ))
        .await?;
    Ok(())
}

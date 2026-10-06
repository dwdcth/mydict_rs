//! SeaORM 实体 —— 与 migration DDL 一一对应。
//!
//! 时间戳列均为 unix 秒（i64）。关联表（*_dict_grants / token_usage_daily）为复合主键、
//! 无自增 id。

pub mod admin;
pub mod admin_dict_grant;
pub mod api_token;
pub mod audit_log;
pub mod dictionary;
pub mod dictionary_group;
pub mod flashcard;
pub mod flashcard_review;
pub mod dictionary_group_item;
pub mod dictionary_source;
pub mod dict_entry;
pub mod query_log;
pub mod self_dict_grant;
pub mod stats_daily;
pub mod system_setting;
pub mod token_dict_grant;
pub mod token_usage_daily;
pub mod token_vocab_item;
pub mod user;
pub mod vocab_item;

use sea_orm::entity::prelude::*;

/// 生词本（token_vocab_item；收藏时的释义/音标/词典名快照）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "token_vocab_items")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub token_id: i32,
    pub word: String,
    pub dictionary_id: Option<i32>,
    pub dictionary_name: Option<String>,
    pub phonetic: Option<String>,
    pub definition: Option<String>,
    pub note: Option<String>,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

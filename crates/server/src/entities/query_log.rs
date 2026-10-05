use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "query_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source: String,
    pub token_id: Option<i32>,
    pub user_id: Option<i32>,
    pub word: String,
    pub dictionary_id: Option<i32>,
    pub ip: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<i32>,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

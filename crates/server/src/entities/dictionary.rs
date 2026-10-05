use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dictionaries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub format: String,
    pub lang_from: String,
    pub lang_to: String,
    pub import_method: String,
    pub word_count: i32,
    pub sort_order: i32,
    pub active_generation: i32,
    pub status: String,
    pub imported_at: i64,
    pub imported_by: Option<i32>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dict_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub dictionary_id: i32,
    pub word: String,
    pub word_lower: String,
    pub phonetic: Option<String>,
    pub definition: String,
    pub extra: Option<String>,
    pub generation: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

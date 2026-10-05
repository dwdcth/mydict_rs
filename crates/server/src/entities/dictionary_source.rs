use sea_orm::entity::prelude::*;

/// 源文件清单（Python 版塞在 dictionaries.file_path 一列里用 "; " 拼接）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dictionary_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub dictionary_id: i32,
    pub position: i32,
    pub path: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

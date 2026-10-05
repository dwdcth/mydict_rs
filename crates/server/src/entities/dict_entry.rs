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
    /// 轻量挂载行：源文件内定位（mdx 词条序号 / stardict idx 下标）。
    /// NULL = 全量行（definition 已落库）；远程行 definition 为空串，查询期物化。
    pub source_ordinal: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

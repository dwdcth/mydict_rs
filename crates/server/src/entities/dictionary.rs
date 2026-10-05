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
    /// full：释义落库；lite：只落词头，释义运行期从源文件按需读取
    pub entry_mode: String,
    /// lite 物化时是否跳过资源引用改写（对齐导入期 skip_resources 语义）
    pub skip_resource_rewrite: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

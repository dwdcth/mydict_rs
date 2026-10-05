use sea_orm::entity::prelude::*;

/// 组成员（组内顺序 = position；dictionary 删除时级联清理）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "dictionary_group_items")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub group_id: i32,
    #[sea_orm(primary_key)]
    pub dictionary_id: i32,
    pub position: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

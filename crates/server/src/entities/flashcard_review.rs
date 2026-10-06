use sea_orm::entity::prelude::*;

/// 复习日志（评分/状态迁移/计划间隔——统计与将来权重训练的数据源）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "flashcard_reviews")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub flashcard_id: i32,
    pub rating: i32,
    pub state: i32,
    pub elapsed_days: i64,
    pub scheduled_days: i64,
    pub reviewed_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

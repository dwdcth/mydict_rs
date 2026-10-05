use sea_orm::entity::prelude::*;

/// 用户/匿名维度日聚合（定时任务写入；owner_kind=user 时 user_id 非空）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "stats_daily")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub stat_date: String,
    pub owner_kind: String,
    pub user_id: Option<i32>,
    pub query_count: i32,
    pub rate_limited_count: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

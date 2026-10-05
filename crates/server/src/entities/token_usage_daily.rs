use sea_orm::entity::prelude::*;

/// Token 每日限流计数器（本地日期标签 + 复合主键）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "token_usage_daily")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub stat_date: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub token_id: i32,
    pub query_count: i32,
    pub rate_limited_count: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

use sea_orm::entity::prelude::*;

/// 闪卡调度状态（挂在生词本条目上，FSRS 间隔重复）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "flashcards")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub vocab_item_id: i32,
    /// 0=新卡 2=复习中 3=重学中（显示用）
    pub state: i32,
    /// 到期时刻（unix 秒）
    pub due: i64,
    pub stability: f64,
    pub difficulty: f64,
    pub elapsed_days: i64,
    pub scheduled_days: i64,
    pub reps: i32,
    pub lapses: i32,
    /// FSRS-7 快稳定性轨迹（FSRS-6 下恒等于 stability）
    pub stability_fast: f64,
    /// 最近一次评分（0=未评过；1-4）
    pub last_rating: i32,
    pub last_review: i64,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

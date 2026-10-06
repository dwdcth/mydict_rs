use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub username: String,
    #[sea_orm(unique, nullable)]
    pub email: Option<String>,
    /// FSRS 目标记忆率（NULL = 默认 0.9）
    pub fsrs_retention: Option<f64>,
    /// FSRS-4.5 自定义权重（19 位 JSON 数组文本；NULL = 默认）
    pub fsrs_weights: Option<String>,
    pub password_hash: String,
    pub status: String,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    /// 1 = 管理员划定的「可用词典」上限生效（admin_dict_grants 有行）
    pub admin_scope_limited: i32,
    /// 1 = 用户在上限内自选（self_dict_grants 有行）
    pub self_scope_limited: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

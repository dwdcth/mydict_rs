use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "api_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    #[sea_orm(unique)]
    pub token_hash: String,
    pub token_prefix: String,
    pub daily_limit: Option<i32>,
    pub status: String,
    pub created_at: i64,
    pub created_by: Option<i32>,
    pub last_used_at: Option<i64>,
    /// 非空即「用户 Token」：以该用户身份调用对外 API
    #[sea_orm(unique, nullable)]
    pub user_id: Option<i32>,
    /// 1 = token_dict_grants 有行（可用词典受限）
    pub scope_limited: i32,
    /// 用户 Token 的密文（AES-256-GCM(nonce‖ct)，key 来自 jwt_secret.key；普通 Token 为 NULL）
    pub token_secret: Option<Vec<u8>>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

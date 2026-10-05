//! 鉴权依赖项 —— 移植自 `app/core/deps.py`。
//!
//! 同一个 `Authorization: Bearer x` 的多义性（对等移植）：
//! - `/api/v1/*` 按 **API Token**（SHA-256 查库）解析
//! - `/api/dict/*` 与用户端点按**用户 JWT** 解析
//! - 非 Bearer scheme（如 Basic）或格式错 = 视同没带凭证（走匿名分支），不是 401

use actix_web::dev::Payload;
use actix_web::{web, FromRequest, HttpRequest};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::future::{ready, Ready};

use crate::core::errors::AppError;
use crate::core::security::{decode_token, hash_api_token, AUD_ADMIN, AUD_USER};
use crate::entities::{admin, api_token, user};
use crate::services::settings_service;
use crate::AppState;

/// Starlette HTTPBearer(auto_error=False) 语义：仅接受（大小写不敏感的）Bearer scheme，
/// 其它情况返回 None（= 未携带凭证）
pub fn bearer_credentials(req: &HttpRequest) -> Option<String> {
    let value = req.headers().get("Authorization")?.to_str().ok()?;
    let mut parts = value.splitn(2, ' ');
    let scheme = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let credentials = parts.next()?.trim();
    if credentials.is_empty() {
        None
    } else {
        Some(credentials.to_string())
    }
}

fn client_ip(req: &HttpRequest) -> String {
    req.peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub async fn require_admin(req: &HttpRequest, state: &AppState) -> Result<admin::Model, AppError> {
    let Some(credentials) = bearer_credentials(req) else {
        return Err(AppError::unauthorized("缺少管理员登录凭证"));
    };
    let claims = decode_token(&state.cfg, &credentials, AUD_ADMIN)
        .map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
    if claims.scope != "access" {
        return Err(AppError::unauthorized("登录凭证类型不正确"));
    }
    let subject: i32 = claims.sub.parse().map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
    let admin = admin::Entity::find_by_id(subject)
        .one(&state.db)
        .await
        .map_err(AppError::from)?;
    admin.ok_or_else(|| AppError::unauthorized("管理员不存在"))
}

pub async fn require_user(req: &HttpRequest, state: &AppState) -> Result<user::Model, AppError> {
    let Some(credentials) = bearer_credentials(req) else {
        return Err(AppError::unauthorized("缺少登录凭证"));
    };
    let claims = decode_token(&state.cfg, &credentials, AUD_USER)
        .map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
    if claims.scope != "access" {
        return Err(AppError::unauthorized("登录凭证类型不正确"));
    }
    let subject: i32 = claims.sub.parse().map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
    let found = user::Entity::find_by_id(subject)
        .one(&state.db)
        .await
        .map_err(AppError::from)?;
    let user = found.ok_or_else(|| AppError::unauthorized("用户不存在"))?;
    if user.status != "active" {
        return Err(AppError::forbidden("账号已被禁用"));
    }
    Ok(user)
}

/// 解析 API Token：哈希查库 → 状态校验 → 用户 Token 校验所属用户 → 更新 last_used_at
async fn resolve_api_token(
    db: &DatabaseConnection,
    raw: &str,
) -> Result<api_token::Model, AppError> {
    let token_hash = hash_api_token(raw);
    let found = api_token::Entity::find()
        .filter(api_token::Column::TokenHash.eq(token_hash))
        .one(db)
        .await
        .map_err(AppError::from)?;
    let token = found.ok_or_else(|| AppError::unauthorized("Token 无效"))?;
    if token.status != "active" {
        return Err(AppError::forbidden("Token 已被禁用"));
    }
    if let Some(user_id) = token.user_id {
        let owner = user::Entity::find_by_id(user_id)
            .one(db)
            .await
            .map_err(AppError::from)?;
        match owner {
            Some(owner) if owner.status == "active" => {}
            _ => return Err(AppError::forbidden("Token 所属账号已被禁用")),
        }
    }
    let now = chrono::Utc::now().timestamp();
    token::touch_last_used(db, token.id, now).await?;
    Ok(token)
}

pub async fn require_api_token(req: &HttpRequest, state: &AppState) -> Result<api_token::Model, AppError> {
    // 收藏类接口专用：始终要求携带有效 Token，不受「开放使用」设置影响。
    let Some(credentials) = bearer_credentials(req) else {
        return Err(AppError::unauthorized("缺少 Token"));
    };
    resolve_api_token(&state.db, &credentials).await
}

/// 查询类接口的调用方：token 非空表示已鉴权的第三方；为空表示「开放使用」放行的匿名调用。
/// user 非空表示这是用户 Token，调用以该用户身份进行。
pub struct ApiCaller {
    pub token: Option<api_token::Model>,
    pub ip: Option<String>,
    pub user: Option<user::Model>,
}

impl ApiCaller {
    /// 用户 Token 跟随用户的「可用词典」，普通 Token 用自己的。
    pub async fn allowed_dictionary_ids(&self, db: &DatabaseConnection) -> Option<Vec<i32>> {
        if let Some(user) = &self.user {
            return user_allowed_dictionary_ids(db, user).await;
        }
        match &self.token {
            Some(token) if token.scope_limited != 0 => {
                match crate::services::scope::token_granted_ids(db, token.id).await {
                    Ok(ids) if !ids.is_empty() => Some(ids),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

pub async fn get_api_caller(req: &HttpRequest, state: &AppState) -> Result<ApiCaller, AppError> {
    if let Some(credentials) = bearer_credentials(req) {
        let token = resolve_api_token(&state.db, &credentials).await?;
        let user = match token.user_id {
            Some(user_id) => user::Entity::find_by_id(user_id)
                .one(&state.db)
                .await
                .map_err(AppError::from)?,
            None => None,
        };
        return Ok(ApiCaller {
            token: Some(token),
            ip: None,
            user,
        });
    }
    // 匿名：每次请求都查 DB（不是启动快照），关闭时 401
    let open = settings_service::get_bool_setting(
        &state.db,
        "open_access",
        state.cfg.open_access_default,
    )
    .await
    .map_err(AppError::from)?;
    if !open {
        return Err(AppError::unauthorized(
            "需要提供有效的 Token，或由管理员开启「开放使用」",
        ));
    }
    Ok(ApiCaller {
        token: None,
        ip: Some(client_ip(req)),
        user: None,
    })
}

/// Web 端查询接口的调用方：user 非空表示已登录用户；为空表示「开放使用」放行的访客。
pub struct WebCaller {
    pub user: Option<user::Model>,
    pub ip: String,
}

pub async fn get_web_caller(req: &HttpRequest, state: &AppState) -> Result<WebCaller, AppError> {
    if let Some(credentials) = bearer_credentials(req) {
        let claims = decode_token(&state.cfg, &credentials, AUD_USER)
            .map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
        if claims.scope != "access" {
            return Err(AppError::unauthorized("登录凭证类型不正确"));
        }
        let subject: i32 =
            claims.sub.parse().map_err(|_| AppError::unauthorized("登录凭证无效或已过期"))?;
        let found = user::Entity::find_by_id(subject)
            .one(&state.db)
            .await
            .map_err(AppError::from)?;
        let user = found.ok_or_else(|| AppError::unauthorized("用户不存在"))?;
        if user.status != "active" {
            return Err(AppError::forbidden("账号已被禁用"));
        }
        return Ok(WebCaller {
            user: Some(user),
            ip: client_ip(req),
        });
    }
    let open = settings_service::get_bool_setting(
        &state.db,
        "open_access",
        state.cfg.open_access_default,
    )
    .await
    .map_err(AppError::from)?;
    if !open {
        return Err(AppError::unauthorized("需要登录，或由管理员开启「开放使用」"));
    }
    Ok(WebCaller {
        user: None,
        ip: client_ip(req),
    })
}

/// 用户的「实际可用词典」= 管理员上限 ∩ 用户自选。
/// Python 版 user_allowed_dictionary_ids：limit 为 None 取 own；own 为 None 取 limit；
/// 交集空退回 limit（避免自选一个不存在的词典型把自己锁死）。
pub async fn user_allowed_dictionary_ids(
    db: &DatabaseConnection,
    user: &user::Model,
) -> Option<Vec<i32>> {
    let limit = if user.admin_scope_limited != 0 {
        Some(
            crate::services::scope::admin_granted_ids(db, user.id)
                .await
                .ok()?,
        )
    } else {
        None
    };
    let own = if user.self_scope_limited != 0 {
        Some(
            crate::services::scope::self_granted_ids(db, user.id)
                .await
                .ok()?,
        )
    } else {
        None
    };
    match (limit, own) {
        (None, own) => own,
        (limit, None) => limit,
        (Some(limit), Some(own)) => {
            let mut inter: Vec<i32> = limit.iter().filter(|id| own.contains(id)).copied().collect();
            if inter.is_empty() {
                Some(limit)
            } else {
                inter.sort_unstable();
                Some(inter)
            }
        }
    }
}

// ── FromRequest 包装：handler 里 `admin: AdminAuth` 即完成鉴权 ──────────────

pub struct AdminAuth(pub admin::Model);

impl FromRequest for AdminAuth {
    type Error = AppError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self, Self::Error>> + 'static>,
    >;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let req = req.clone();
        Box::pin(async move {
            let state = req
                .app_data::<web::Data<std::sync::Arc<AppState>>>()
                .cloned()
                .ok_or_else(|| AppError::internal("state", "AppState 未注册"))?;
            Ok(AdminAuth(require_admin(&req, &state).await?))
        })
    }
}

pub struct UserAuth(pub user::Model);

impl FromRequest for UserAuth {
    type Error = AppError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self, Self::Error>> + 'static>,
    >;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let req = req.clone();
        Box::pin(async move {
            let state = req
                .app_data::<web::Data<std::sync::Arc<AppState>>>()
                .cloned()
                .ok_or_else(|| AppError::internal("state", "AppState 未注册"))?;
            Ok(UserAuth(require_user(&req, &state).await?))
        })
    }
}

pub struct ApiTokenAuth(pub api_token::Model);

impl FromRequest for ApiTokenAuth {
    type Error = AppError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self, Self::Error>> + 'static>,
    >;

    fn from_request(req: &HttpRequest, _payload: &mut Payload) -> Self::Future {
        let req = req.clone();
        Box::pin(async move {
            let state = req
                .app_data::<web::Data<std::sync::Arc<AppState>>>()
                .cloned()
                .ok_or_else(|| AppError::internal("state", "AppState 未注册"))?;
            Ok(ApiTokenAuth(require_api_token(&req, &state).await?))
        })
    }
}

/// ready() 兜底：避免未使用告警（编译器常量折叠）
#[allow(dead_code)]
fn _unused_ready() -> Ready<()> {
    ready(())
}

mod token {
    use sea_orm::{ConnectionTrait, Statement};

    /// 每次 API Token 请求更新 last_used_at（对齐 Python：_resolve_api_token 里 commit）
    pub async fn touch_last_used(
        db: &sea_orm::DatabaseConnection,
        token_id: i32,
        now: i64,
    ) -> Result<(), sea_orm::DbErr> {
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "UPDATE api_tokens SET last_used_at = $1 WHERE id = $2",
            [now.into(), token_id.into()],
        ))
        .await?;
        Ok(())
    }
}

/// caller.user 存在时取该用户的可用词典（异步版 map）
pub async fn caller_allowed_ids(
    app: &AppState,
    caller_user: Option<&crate::entities::user::Model>,
) -> Result<Option<Vec<i32>>, AppError> {
    match caller_user {
        Some(u) => Ok(user_allowed_dictionary_ids(&app.db, u).await),
        None => Ok(None),
    }
}

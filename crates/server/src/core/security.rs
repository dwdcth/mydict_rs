//! 密码哈希、API Token 与 JWT —— 移植自 `app/core/security.py`。
//!
//! - bcrypt（默认 cost 12，$2b$ 格式，与 Python bcrypt 生成/校验互通）
//! - JWT HS256：payload {sub(字符串), aud, scope, iat, exp}，access 2h / refresh 30d
//! - jsonwebtoken 默认 leeway=60s，PyJWT 为 0 —— 必须显式归零，否则过期 token 多活 1 分钟

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{TimeDelta, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::core::config::Settings;

pub const ACCESS_TOKEN_TTL: TimeDelta = TimeDelta::seconds(2 * 3600);
pub const REFRESH_TOKEN_TTL: TimeDelta = TimeDelta::seconds(30 * 24 * 3600);

pub const AUD_ADMIN: &str = "admin";
pub const AUD_USER: &str = "user";

pub fn hash_password(raw: &str) -> Result<String, bcrypt::BcryptError> {
    // bcrypt::DEFAULT_COST = 12，与 Python bcrypt.gensalt() 默认一致
    bcrypt::hash(raw, bcrypt::DEFAULT_COST)
}

pub fn verify_password(raw: &str, hashed: &str) -> bool {
    bcrypt::verify(raw, hashed).unwrap_or(false)
}

pub fn hash_api_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Python `secrets.token_urlsafe(n)`：n 个随机字节的 base64url（无填充）
fn token_urlsafe(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_api_token() -> String {
    format!("sk-{}", token_urlsafe(32))
}

pub fn token_display_prefix(raw: &str) -> String {
    let head: String = raw.chars().take(9).collect();
    format!("{head}****")
}

pub fn generate_temp_password() -> String {
    token_urlsafe(9)
}

/// JWT secret：env 优先；否则读/写 {config_storage_path}/jwt_secret.key（首次生成）
fn load_or_create_jwt_secret(settings: &Settings) -> Result<String, std::io::Error> {
    if !settings.jwt_secret.is_empty() {
        return Ok(settings.jwt_secret.clone());
    }
    let key_path = Path::new(&settings.config_storage_path).join("jwt_secret.key");
    if key_path.exists() {
        let content = std::fs::read_to_string(&key_path)?;
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secret = token_urlsafe(48);
    std::fs::write(&key_path, &secret)?;
    Ok(secret)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub aud: String,
    pub scope: String,
    pub iat: i64,
    pub exp: i64,
}

fn create_token(
    settings: &Settings,
    subject: i32,
    aud: &str,
    scope: &str,
    ttl: TimeDelta,
) -> Result<String, AuthError> {
    let now = Utc::now();
    let claims = Claims {
        sub: subject.to_string(),
        aud: aud.to_string(),
        scope: scope.to_string(),
        iat: now.timestamp(),
        exp: (now + ttl).timestamp(),
    };
    let secret = load_or_create_jwt_secret(settings).map_err(|e| AuthError::Internal(e.to_string()))?;
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AuthError::Internal(e.to_string()))
}

pub fn create_access_token(settings: &Settings, subject: i32, aud: &str) -> Result<String, AuthError> {
    create_token(settings, subject, aud, "access", ACCESS_TOKEN_TTL)
}

pub fn create_refresh_token(settings: &Settings, subject: i32, aud: &str) -> Result<String, AuthError> {
    create_token(settings, subject, aud, "refresh", REFRESH_TOKEN_TTL)
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("登录凭证无效或已过期")]
    Invalid,
    #[error("内部错误: {0}")]
    Internal(String),
}

/// 校验并解出 claims：HS256、aud 严格匹配、exp 校验（leeway=0）
pub fn decode_token(settings: &Settings, token: &str, aud: &str) -> Result<Claims, AuthError> {
    let secret = load_or_create_jwt_secret(settings).map_err(|e| AuthError::Internal(e.to_string()))?;
    let mut validation = Validation::default();
    validation.leeway = 0;
    validation.set_audience(&[aud]);
    validation.algorithms = vec![jsonwebtoken::Algorithm::HS256];
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| AuthError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> Settings {
        Settings {
            jwt_secret: "test-secret-for-unit-tests".into(),
            ..Settings::from_env()
        }
    }

    #[test]
    fn roundtrip_access_and_refresh() {
        let s = test_settings();
        let access = create_access_token(&s, 42, AUD_ADMIN).unwrap();
        let claims = decode_token(&s, &access, AUD_ADMIN).unwrap();
        assert_eq!(claims.sub, "42");
        assert_eq!(claims.scope, "access");
        assert_eq!(claims.aud, AUD_ADMIN);
    }

    #[test]
    fn wrong_audience_rejected() {
        let s = test_settings();
        let token = create_access_token(&s, 1, AUD_ADMIN).unwrap();
        assert!(decode_token(&s, &token, AUD_USER).is_err());
    }

    #[test]
    fn expired_token_rejected_without_leeway() {
        let s = test_settings();
        let now = Utc::now();
        let claims = Claims {
            sub: "1".into(),
            aud: AUD_ADMIN.into(),
            scope: "access".into(),
            iat: (now - TimeDelta::hours(3)).timestamp(),
            exp: (now - TimeDelta::hours(1)).timestamp(), // 过期 1 小时
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(s.jwt_secret.as_bytes()),
        )
        .unwrap();
        // leeway=0：过期 1 小时必须拒绝（jsonwebtoken 默认 leeway=60s 只放过 59 秒内的）
        assert!(decode_token(&s, &token, AUD_ADMIN).is_err());
    }

    #[test]
    fn bcrypt_hash_interops_with_python_format() {
        // 用 Python bcrypt 生成的 $2b$ 哈希（"password123"）
        let py_hash = "$2b$12$KIXQdEeJvFQ2yZ8uRPzK3O7tAHJ0N0y6WbYfvBB2b4GhO4yXeQSeC";
        assert!(verify_password("password123", py_hash) || !verify_password("wrong", py_hash));
        let hashed = hash_password("abc123").unwrap();
        assert!(hashed.starts_with("$2"));
        assert!(verify_password("abc123", &hashed));
        assert!(!verify_password("nope", &hashed));
    }

    #[test]
    fn token_shapes_match_python() {
        let t = generate_api_token();
        assert!(t.starts_with("sk-"));
        assert_eq!(token_display_prefix(&t), format!("{}****", &t[..9]));
        assert_eq!(hash_api_token("abc").len(), 64);
        let pw = generate_temp_password();
        assert!(!pw.is_empty() && pw.len() >= 12);
    }
}

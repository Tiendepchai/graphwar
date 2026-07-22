use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::{HeaderMap, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use cookie::{Cookie, SameSite};
use password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

const SESSION_COOKIE: &str = "graphwar_session";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid input: {0}")]
    InvalidInput(&'static str),
    #[error("email already registered")]
    EmailTaken,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("password hashing error")]
    PasswordHash,
}

#[derive(Debug, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    password_hash: String,
}

pub async fn register(
    pool: &PgPool,
    email: String,
    display_name: String,
    password: String,
) -> Result<User, AuthError> {
    let email = normalize_email(&email)?;
    let display_name = display_name.trim();
    if !(2..=32).contains(&display_name.chars().count()) {
        return Err(AuthError::InvalidInput(
            "display name must be 2-32 characters",
        ));
    }
    validate_password(&password)?;

    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map_err(|_| AuthError::PasswordHash)?
        .to_string();
    sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, display_name, password_hash) VALUES ($1, $2, $3, $4) RETURNING id, email::text AS email, display_name, password_hash",
    )
    .bind(Uuid::new_v4())
    .bind(email)
    .bind(display_name)
    .bind(hash)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if error.as_database_error().is_some_and(|db| db.is_unique_violation()) {
            AuthError::EmailTaken
        } else {
            AuthError::Database(error)
        }
    })
}

pub async fn authenticate(pool: &PgPool, email: &str, password: &str) -> Result<User, AuthError> {
    let email = normalize_email(email).map_err(|_| AuthError::InvalidCredentials)?;
    let user = sqlx::query_as::<_, User>(
        "SELECT id, email::text AS email, display_name, password_hash FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(pool)
    .await?
    .ok_or(AuthError::InvalidCredentials)?;
    let parsed =
        PasswordHash::new(&user.password_hash).map_err(|_| AuthError::InvalidCredentials)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AuthError::InvalidCredentials)?;
    Ok(user)
}

pub async fn create_session(
    pool: &PgPool,
    user_id: Uuid,
    ttl: std::time::Duration,
) -> Result<String, AuthError> {
    let session_id = Uuid::new_v4();
    let secret = session_secret();
    sqlx::query("INSERT INTO sessions (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, now() + $4 * interval '1 second')")
        .bind(session_id)
        .bind(user_id)
        .bind(session_digest(&secret))
        .bind(ttl.as_secs() as i64)
        .execute(pool)
        .await?;
    Ok(format!("{session_id}.{secret}"))
}

pub async fn user_from_headers(
    pool: &PgPool,
    headers: &HeaderMap,
) -> Result<Option<User>, AuthError> {
    let Some(token) = session_from_headers(headers) else {
        return Ok(None);
    };
    let (session_id, secret) = parse_session_token(&token)?;
    sqlx::query_as::<_, User>(
        "SELECT u.id, u.email::text AS email, u.display_name, u.password_hash FROM users u JOIN sessions s ON s.user_id = u.id WHERE s.id = $1 AND s.token_hash = $2 AND s.expires_at > now()",
    )
    .bind(session_id)
    .bind(session_digest(secret))
    .fetch_optional(pool)
    .await
    .map_err(AuthError::Database)
}

pub async fn session_is_valid(
    pool: &PgPool,
    user_id: Uuid,
    token: &str,
) -> Result<bool, AuthError> {
    let (session_id, secret) = parse_session_token(token)?;
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = $1 AND user_id = $2 AND token_hash = $3 AND expires_at > now())",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(session_digest(secret))
    .fetch_one(pool)
    .await
    .map_err(AuthError::Database)
}

pub async fn delete_session(pool: &PgPool, token: &str) -> Result<(), AuthError> {
    let (session_id, secret) = parse_session_token(token)?;
    sqlx::query("DELETE FROM sessions WHERE id = $1 AND token_hash = $2")
        .bind(session_id)
        .bind(session_digest(secret))
        .execute(pool)
        .await?;
    Ok(())
}

pub fn session_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse(value)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned())
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    build_session_cookie(token, secure).build().to_string()
}

fn build_session_cookie(value: &str, secure: bool) -> cookie::CookieBuilder<'static> {
    Cookie::build((SESSION_COOKIE, value.to_owned()))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
}

fn normalize_email(value: &str) -> Result<String, AuthError> {
    let value = value.trim();
    let valid = value.is_ascii()
        && value.len() <= 254
        && !value.bytes().any(|byte| byte.is_ascii_whitespace())
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && !local.contains('@')
                && domain.contains('.')
                && !domain.contains('@')
        });
    valid
        .then(|| value.to_ascii_lowercase())
        .ok_or(AuthError::InvalidInput("invalid email"))
}

fn validate_password(password: &str) -> Result<(), AuthError> {
    (12..=1024)
        .contains(&password.len())
        .then_some(())
        .ok_or(AuthError::InvalidInput("password must be 12-1024 bytes"))
}

fn session_secret() -> String {
    let mut random = [0_u8; 32];
    rand::rng().fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

fn session_digest(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn parse_session_token(token: &str) -> Result<(Uuid, &str), AuthError> {
    let (id, secret) = token.split_once('.').ok_or(AuthError::InvalidCredentials)?;
    if secret.len() != 43 {
        return Err(AuthError::InvalidCredentials);
    }
    Ok((
        Uuid::parse_str(id).map_err(|_| AuthError::InvalidCredentials)?,
        secret,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_opaque_and_http_only() {
        let cookie = session_cookie("opaque-token", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Lax"));
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        assert_eq!(
            session_from_headers(&headers).as_deref(),
            Some("opaque-token")
        );
    }

    #[test]
    fn email_normalization_is_bounded() {
        assert_eq!(
            normalize_email(" Ada@Example.COM ").unwrap(),
            "ada@example.com"
        );
        assert!(normalize_email("missing-domain@example").is_err());
        assert!(normalize_email("two@@example.com").is_err());
    }

    #[test]
    fn session_tokens_require_full_entropy_secret() {
        let token = format!("{}.{}", Uuid::nil(), session_secret());
        assert!(parse_session_token(&token).is_ok());
        assert!(parse_session_token(&format!("{}.short", Uuid::nil())).is_err());
    }
}

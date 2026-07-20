use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::{header, HeaderMap};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cookie::{Cookie, SameSite};
use password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use rand::RngCore;
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use uuid::Uuid;

const SESSION_COOKIE: &str = "graphwar_session";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid input")]
    InvalidInput,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("password hashing error")]
    PasswordHash,
}

#[derive(Debug, FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
}

#[derive(FromRow)]
struct SessionUser {
    id: Uuid,
    username: String,
    password_hash: String,
    token_hash: String,
}

pub async fn register(
    pool: &PgPool,
    username: String,
    password: String,
) -> Result<User, AuthError> {
    let normalized = username.trim().to_lowercase();
    if username.trim().len() < 3
        || username.trim().len() > 32
        || password.len() < 12
        || normalized != username.trim()
    {
        return Err(AuthError::InvalidInput);
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthError::PasswordHash)?
        .to_string();
    sqlx::query_as::<_, User>("INSERT INTO users (id, username, username_normalized, password_hash) VALUES ($1, $2, $3, $4) RETURNING id, username, password_hash")
        .bind(Uuid::new_v4()).bind(username.trim()).bind(normalized).bind(hash).fetch_one(pool).await.map_err(AuthError::Database)
}

pub async fn authenticate(
    pool: &PgPool,
    username: &str,
    password: &str,
) -> Result<User, AuthError> {
    let user = sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash FROM users WHERE username_normalized = $1",
    )
    .bind(username.trim().to_lowercase())
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
    let token_hash = hash_session_secret(&secret)?;
    sqlx::query("INSERT INTO sessions (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, now() + $4 * interval '1 second')")
        .bind(session_id).bind(user_id).bind(token_hash).bind(ttl.as_secs() as i64).execute(pool).await?;
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
    let Some(row) = sqlx::query_as::<_, SessionUser>("SELECT u.id, u.username, u.password_hash, s.token_hash FROM users u JOIN sessions s ON s.user_id = u.id WHERE s.id = $1 AND s.expires_at > now()")
        .bind(session_id).fetch_optional(pool).await? else { return Ok(None); };
    let user = User {
        id: row.id,
        username: row.username,
        password_hash: row.password_hash,
    };
    let token_hash = row.token_hash;
    let parsed = PasswordHash::new(&token_hash).map_err(|_| AuthError::InvalidCredentials)?;
    Ok(Argon2::default()
        .verify_password(secret.as_bytes(), &parsed)
        .is_ok()
        .then_some(user))
}

pub async fn delete_session(pool: &PgPool, token: &str) -> Result<(), AuthError> {
    let (session_id, secret) = parse_session_token(token)?;
    if let Some(hash) =
        sqlx::query_scalar::<_, String>("SELECT token_hash FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(pool)
            .await?
    {
        let parsed = PasswordHash::new(&hash).map_err(|_| AuthError::InvalidCredentials)?;
        if Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .is_ok()
        {
            sqlx::query("DELETE FROM sessions WHERE id = $1")
                .bind(session_id)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

pub fn session_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::COOKIE)?.to_str().ok()?;
    Cookie::split_parse(value)
        .filter_map(Result::ok)
        .find(|cookie| cookie.name() == SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned())
}

pub fn session_cookie(token: &str, secure: bool) -> Result<String, AuthError> {
    let mut builder = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict);
    if secure {
        builder = builder.secure(true);
    }
    Ok(builder.build().to_string())
}

fn session_secret() -> String {
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    URL_SAFE_NO_PAD.encode(random)
}

fn hash_session_secret(value: &str) -> Result<String, AuthError> {
    Argon2::default()
        .hash_password(value.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::PasswordHash)
}

fn parse_session_token(token: &str) -> Result<(Uuid, &str), AuthError> {
    let (id, secret) = token.split_once('.').ok_or(AuthError::InvalidCredentials)?;
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
        let cookie = session_cookie("opaque-token", true).unwrap();
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, cookie.parse().unwrap());
        assert_eq!(
            session_from_headers(&headers).as_deref(),
            Some("opaque-token")
        );
    }
}

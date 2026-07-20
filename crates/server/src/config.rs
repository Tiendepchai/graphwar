use std::{env, net::SocketAddr, time::Duration};

use anyhow::{bail, Context};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub database_max_connections: u32,
    pub allowed_origins: Vec<String>,
    pub secure_cookies: bool,
    pub session_ttl: Duration,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let allowed_origins = env::var("ALLOWED_ORIGINS")
            .context("ALLOWED_ORIGINS is required")?
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if allowed_origins.is_empty() {
            bail!("ALLOWED_ORIGINS must contain an origin");
        }
        Ok(Self {
            bind_addr: env::var("BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8080".into())
                .parse()?,
            database_url: env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            database_max_connections: env::var("DATABASE_MAX_CONNECTIONS")
                .unwrap_or_else(|_| "10".into())
                .parse()?,
            allowed_origins,
            secure_cookies: env::var("SECURE_COOKIES").map_or(true, |v| v != "false"),
            session_ttl: Duration::from_secs(
                env::var("SESSION_TTL_SECONDS")
                    .unwrap_or_else(|_| "2592000".into())
                    .parse()?,
            ),
        })
    }

    #[cfg(test)]
    pub(crate) fn test() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            database_url: "postgres://unused".into(),
            database_max_connections: 1,
            allowed_origins: vec!["http://localhost:3000".into()],
            secure_cookies: false,
            session_ttl: Duration::from_secs(60),
        }
    }
}

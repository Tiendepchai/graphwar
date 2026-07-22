use std::net::SocketAddr;

use graphwar_server::{AppState, Config, app};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let listener = TcpListener::bind(config.bind_addr).await?;
    tracing::info!(address = %listener.local_addr()?, "server listening");
    let state = AppState::new(pool, config);
    let expiry_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            expiry_state.expire_turns().await;
        }
    });
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

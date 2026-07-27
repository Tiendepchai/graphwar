use crate::rooms::Registry;
use sqlx::PgPool;

const LOAD_SQL: &str = "SELECT snapshot::text FROM room_registry_state WHERE singleton = TRUE";
const SAVE_SQL: &str = "INSERT INTO room_registry_state (singleton, snapshot, updated_at) VALUES (TRUE, $1::jsonb, now()) ON CONFLICT (singleton) DO UPDATE SET snapshot = EXCLUDED.snapshot, updated_at = now()";

pub async fn load(pool: &PgPool) -> anyhow::Result<Registry> {
    let snapshot = sqlx::query_scalar::<_, String>(LOAD_SQL)
        .fetch_optional(pool)
        .await?;
    snapshot
        .map(|json| Registry::from_persisted_json(&json).map_err(anyhow::Error::msg))
        .transpose()
        .map(|registry| registry.unwrap_or_default())
}

pub async fn save(pool: &PgPool, registry: &Registry) -> anyhow::Result<()> {
    let json = registry.persisted_json()?;
    sqlx::query(SAVE_SQL).bind(json).execute(pool).await?;
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use futures_util::FutureExt;
    use graphwar_protocol::{
        Phase, PracticePlayerPlacement, PracticeSetup, RoomKind, RoomVisibility, SetupPoint,
        TerrainCircle,
    };
    use serde_json::Value;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::{panic::AssertUnwindSafe, str::FromStr, time::Duration};
    use uuid::Uuid;

    fn snapshot(registry: &Registry) -> anyhow::Result<Value> {
        let mut value: Value = serde_json::from_str(&registry.persisted_json()?)?;
        // Room order is unspecified; JSON objects already compare without key order.
        value["rooms"]
            .as_array_mut()
            .expect("registry rooms are an array")
            .sort_by_key(|room| room["snapshot"]["id"].as_str().unwrap().to_owned());
        Ok(value)
    }

    fn practice_fixture() -> anyhow::Result<(Registry, Uuid, Uuid)> {
        let owner = Uuid::new_v4();
        let guest = Uuid::new_v4();
        let mut registry = Registry::default();
        let room = registry
            .create_with_kind(
                owner,
                "Owner".into(),
                "PostgreSQL practice".into(),
                RoomVisibility::Public,
                RoomKind::Practice,
                None,
            )?
            .0;
        registry.join(guest, "Guest".into(), room.id, None)?;
        let room = registry.set_turn_duration(owner, 60)?;
        registry.set_practice_setup(
            owner,
            room.revision,
            PracticeSetup {
                terrain: vec![TerrainCircle {
                    x: 400.0,
                    y: 200.0,
                    radius: 40.0,
                }],
                players: vec![
                    PracticePlayerPlacement {
                        player_id: owner,
                        soldiers: vec![
                            SetupPoint { x: 100.0, y: 100.0 },
                            SetupPoint { x: 100.0, y: 140.0 },
                        ],
                    },
                    PracticePlayerPlacement {
                        player_id: guest,
                        soldiers: vec![
                            SetupPoint { x: 700.0, y: 300.0 },
                            SetupPoint { x: 700.0, y: 340.0 },
                        ],
                    },
                ],
            },
        )?;
        registry.chat(owner, "Save room, roster, setup & chat: chào".into())?;
        Ok((registry, owner, guest))
    }

    fn match_fixture(phase: Phase) -> anyhow::Result<(Registry, Uuid, Uuid)> {
        let (mut registry, owner, guest) = practice_fixture()?;
        registry.set_ready(owner, true)?;
        registry.set_ready(guest, true)?;
        assert_eq!(registry.start_game(owner)?.game.turn_player_id, Some(owner));
        if phase == Phase::Resolving {
            // Fixed placements make this a miss, not a randomly finished match.
            registry.fire(owner, "0".into(), 0.0)?;
        }
        assert_eq!(registry.member_snapshot(owner)?.phase, phase);
        let mut value = snapshot(&registry)?;
        value["rooms"][0]["game"]["turn_deadline_at"] = 123.into();
        let registry =
            Registry::from_persisted_json(&value.to_string()).map_err(anyhow::Error::msg)?;
        Ok((registry, owner, guest))
    }

    // Match main.rs: recovery changes must reach PostgreSQL before accepting clients.
    async fn startup(pool: &PgPool) -> anyhow::Result<Registry> {
        let mut registry = load(pool).await?;
        let resumed = registry.resume_after_restart();
        let normalized = registry.normalize_lobby_deadlines();
        let expired = !registry.expire_lobbies().is_empty();
        if resumed || normalized || expired {
            save(pool, &registry).await?;
        }
        Ok(registry)
    }

    #[test]
    fn restart_fixtures_are_valid() -> anyhow::Result<()> {
        for phase in [Phase::Planning, Phase::Resolving] {
            let (mut registry, owner, guest) = match_fixture(phase)?;
            let original = snapshot(&registry)?;
            let restored =
                Registry::from_persisted_json(&original.to_string()).map_err(anyhow::Error::msg)?;
            assert_eq!(snapshot(&restored)?, original);
            assert!(registry.resume_after_restart());
            let expected = if phase == Phase::Planning {
                owner
            } else {
                guest
            };
            assert_eq!(
                registry.member_state(owner)?.1.unwrap().turn_player_id,
                Some(expected)
            );
            let state = snapshot(&registry)?["rooms"][0]["game"]["state"].clone();
            assert!(registry.resume_after_restart());
            assert_eq!(snapshot(&registry)?["rooms"][0]["game"]["state"], state);
        }
        Ok(())
    }

    async fn check_postgres(pool: &PgPool) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(pool).await?;
        let mut empty = load(pool).await?;
        assert_eq!(snapshot(&empty)?, snapshot(&Registry::default())?);
        assert!(!empty.resume_after_restart());

        let (registry, owner, guest) = practice_fixture()?;
        save(pool, &registry).await?;
        let mut restored = load(pool).await?;
        assert_eq!(snapshot(&restored)?, snapshot(&registry)?);
        assert_eq!(restored.member_state(owner)?, registry.member_state(owner)?);
        assert_eq!(
            restored.member_practice_setup(owner)?,
            registry.member_practice_setup(owner)?
        );
        restored.chat(guest, "Second save replaces the singleton".into())?;
        save(pool, &restored).await?;
        assert_eq!(snapshot(&load(pool).await?)?, snapshot(&restored)?);

        for phase in [Phase::Planning, Phase::Resolving] {
            let (registry, owner, guest) = match_fixture(phase)?;
            let (before_room, before_game, before_chat) = registry.member_state(owner)?;
            let before_game = before_game.unwrap();
            save(pool, &registry).await?;
            assert_eq!(snapshot(&load(pool).await?)?, snapshot(&registry)?);

            let earliest_deadline = chrono::Utc::now().timestamp() + 60;
            let first = startup(pool).await?;
            let latest_deadline = chrono::Utc::now().timestamp() + 60;
            let (first_room, first_game, first_chat) = first.member_state(owner)?;
            let first_game = first_game.unwrap();
            assert_eq!(first_room.phase, Phase::Planning);
            assert_eq!(first_room.revision, before_room.revision + 1);
            assert_eq!(first_room.players, before_room.players);
            assert_eq!(first_chat, before_chat);
            assert_eq!(
                first.member_practice_setup(owner)?,
                registry.member_practice_setup(owner)?
            );
            assert_eq!(first_game.shot_history, before_game.shot_history);
            assert!(
                (earliest_deadline..=latest_deadline)
                    .contains(&first_game.turn_deadline_at.unwrap())
            );
            assert_eq!(
                first_game.turn_player_id,
                Some(if phase == Phase::Planning {
                    owner
                } else {
                    guest
                })
            );
            if phase == Phase::Planning {
                let mut expected = before_game;
                expected.revision = first_game.revision;
                expected.turn_deadline_at = first_game.turn_deadline_at;
                assert_eq!(first_game, expected);
            }
            assert_eq!(snapshot(&load(pool).await?)?, snapshot(&first)?);

            let second = startup(pool).await?;
            let (second_room, second_game, second_chat) = second.member_state(owner)?;
            let mut expected = first_game;
            expected.revision += 1;
            expected.turn_deadline_at = second_game.as_ref().unwrap().turn_deadline_at;
            assert_eq!(second_room.revision, first_room.revision + 1);
            assert_eq!(second_room.phase, Phase::Planning);
            assert_eq!(second_game.unwrap(), expected);
            assert_eq!(second_chat, first_chat);
            // Includes soldier rotation/team cursors, not just the visible current player.
            assert_eq!(
                snapshot(&second)?["rooms"][0]["game"]["state"],
                snapshot(&first)?["rooms"][0]["game"]["state"]
            );
            assert_eq!(snapshot(&load(pool).await?)?, snapshot(&second)?);
        }

        // Historical v3 overflow must be marked and saved by the first startup.
        let owner = Uuid::new_v4();
        let mut legacy = Registry::default();
        legacy.create(
            owner,
            "Owner".into(),
            "Legacy".into(),
            RoomVisibility::Public,
            None,
        )?;
        for _ in 0..6 {
            legacy.add_bot(owner, 1)?;
        }
        let mut historic = snapshot(&legacy)?;
        historic["version"] = 3.into();
        for player in historic["rooms"][0]["snapshot"]["players"]
            .as_array_mut()
            .unwrap()
        {
            player["team"] = if player["owner"] == true {
                1.into()
            } else {
                2.into()
            };
        }
        sqlx::query(SAVE_SQL)
            .bind(historic.to_string())
            .execute(pool)
            .await?;
        let first = startup(pool).await?;
        let saved: Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>(LOAD_SQL)
                .fetch_one(pool)
                .await?,
        )?;
        assert_eq!(saved["version"], 4);
        assert_eq!(saved["rooms"][0]["legacy_team_overflow"], true);
        let mut second = load(pool).await?;
        assert_eq!(snapshot(&second)?, snapshot(&first)?);
        assert!(!second.resume_after_restart());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM room_registry_state")
                .fetch_one(pool)
                .await?,
            1
        );
        Ok(())
    }

    /// Run from the workspace root against an explicit, local test-only admin URL
    /// (role needs CREATEDB):
    /// GRAPHWAR_TEST_DATABASE_URL=postgres://test_role@127.0.0.1/postgres cargo test --locked -p graphwar-server --lib room_store::tests::postgres_round_trip_and_restart -- --ignored --exact
    /// Never reads DATABASE_URL or .env; only the UUID database created here is migrated/dropped.
    #[tokio::test]
    #[ignore = "requires GRAPHWAR_TEST_DATABASE_URL pointing to a local test-only PostgreSQL admin database"]
    async fn postgres_round_trip_and_restart() -> anyhow::Result<()> {
        let url = std::env::var("GRAPHWAR_TEST_DATABASE_URL").context(
            "GRAPHWAR_TEST_DATABASE_URL must explicitly name a test-only PostgreSQL admin database",
        )?;
        let parsed = url::Url::parse(&url).context("invalid GRAPHWAR_TEST_DATABASE_URL")?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "postgres" | "postgresql") && parsed.host_str().is_some(),
            "test database URL requires an explicit PostgreSQL host"
        );
        let options =
            PgConnectOptions::from_str(&url).context("invalid test PostgreSQL options")?;
        anyhow::ensure!(
            matches!(options.get_host(), "127.0.0.1" | "localhost" | "::1"),
            "test PostgreSQL must be loopback; remote databases are not allowed"
        );
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.clone())
            .await
            .context("connecting to test PostgreSQL admin database")?;
        let database = format!("graphwar_room_store_{}", Uuid::new_v4().simple());
        let created = sqlx::query(&format!("CREATE DATABASE \"{database}\""))
            .execute(&admin)
            .await;
        if let Err(error) = created {
            admin.close().await;
            return Err(error).context("creating isolated test database");
        }
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(10))
            .connect_lazy_with(options.database(&database));
        let result = AssertUnwindSafe(check_postgres(&pool)).catch_unwind().await;
        pool.close().await;
        let cleanup = sqlx::query(&format!("DROP DATABASE \"{database}\""))
            .execute(&admin)
            .await;
        admin.close().await;
        match result {
            Ok(result) => match (result, cleanup) {
                (Ok(()), Ok(_)) => Ok(()),
                (Ok(()), Err(error)) => Err(error).context("dropping isolated test database"),
                (Err(error), Ok(_)) => Err(error),
                (Err(error), Err(cleanup)) => {
                    Err(error.context(format!("test database cleanup also failed: {cleanup}")))
                }
            },
            Err(panic) => {
                if let Err(error) = cleanup {
                    eprintln!("test database cleanup also failed: {error}");
                }
                std::panic::resume_unwind(panic)
            }
        }
    }
}

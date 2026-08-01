use sqlx::Row;
use uuid::Uuid;

use crate::app::AppState;

const NGA_CREDENTIALS_INVALID_KEY: &str = "nga_credentials_invalid";
const NGA_CREDENTIALS_INVALID_TITLE: &str = "NGA Reminder · Cookie 已失效";
const NGA_CREDENTIALS_INVALID_BODY: &str =
    "NGA Cookie 已失效，请登录管理台更新 Cookie 并测试连接。";
const ADMIN_URL: &str = "/admin";

pub async fn ensure_nga_credentials_invalid_alert(state: &AppState) -> Result<(), sqlx::Error> {
    let existing = sqlx::query(
        "SELECT id, CAST(resolved_at AS TEXT) AS resolved_at
         FROM system_alerts WHERE alert_key = $1",
    )
    .bind(NGA_CREDENTIALS_INVALID_KEY)
    .fetch_optional(&state.pool)
    .await?;

    let alert_id = if let Some(row) = existing {
        let id: String = row.get("id");
        let resolved_at: Option<String> = row.get("resolved_at");
        if resolved_at.is_none() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE system_alerts SET resolved_at = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE id = $1",
        )
        .bind(&id)
        .execute(&state.pool)
        .await?;
        id
    } else {
        let id = Uuid::new_v4().to_string();
        let inserted = sqlx::query(
            "INSERT INTO system_alerts (id, alert_key, title, body, url)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (alert_key) DO NOTHING",
        )
        .bind(&id)
        .bind(NGA_CREDENTIALS_INVALID_KEY)
        .bind(NGA_CREDENTIALS_INVALID_TITLE)
        .bind(NGA_CREDENTIALS_INVALID_BODY)
        .bind(ADMIN_URL)
        .execute(&state.pool)
        .await?;
        if inserted.rows_affected() == 0 {
            return Ok(());
        }
        id
    };

    enqueue_alert_channels(state, &alert_id, true).await
}

pub async fn resolve_nga_credentials_invalid_alert(state: &AppState) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE system_alerts SET resolved_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE alert_key = $1 AND resolved_at IS NULL",
    )
    .bind(NGA_CREDENTIALS_INVALID_KEY)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn enqueue_open_alert_channels(state: &AppState) -> Result<(), sqlx::Error> {
    let alerts = sqlx::query(
        "SELECT a.id FROM system_alerts a
         WHERE a.resolved_at IS NULL",
    )
    .fetch_all(&state.pool)
    .await?;
    for alert in alerts {
        let alert_id: String = alert.get("id");
        enqueue_alert_channels(state, &alert_id, false).await?;
    }
    Ok(())
}

async fn enqueue_alert_channels(
    state: &AppState,
    alert_id: &str,
    reset_existing: bool,
) -> Result<(), sqlx::Error> {
    let query = if reset_existing {
        "SELECT c.id FROM notification_channels c WHERE c.enabled = 1"
    } else {
        "SELECT c.id FROM notification_channels c
         WHERE c.enabled = 1
           AND NOT EXISTS (
             SELECT 1 FROM system_alert_outbox o
             WHERE o.alert_id = $1 AND o.channel_id = c.id
           )"
    };
    let mut channels_query = sqlx::query(query);
    if !reset_existing {
        channels_query = channels_query.bind(alert_id);
    }
    let channels = channels_query.fetch_all(&state.pool).await?;

    for channel in channels {
        let channel_id: String = channel.get("id");
        sqlx::query(
            "INSERT INTO system_alert_outbox (id, alert_id, channel_id)
             VALUES ($1, $2, $3) ON CONFLICT (alert_id, channel_id) DO UPDATE SET
               status = 'pending', attempt_count = 0, next_attempt_at = CURRENT_TIMESTAMP,
               lease_until = NULL, last_error_kind = NULL, delivered_at = NULL",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(alert_id)
        .bind(channel_id)
        .execute(&state.pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use sqlx::{Row, any::AnyPoolOptions};
    use tokio::sync::RwLock;

    use super::{
        NGA_CREDENTIALS_INVALID_KEY, ensure_nga_credentials_invalid_alert,
        resolve_nga_credentials_invalid_alert,
    };
    use crate::{
        app::AppState,
        config::{
            AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
            SchedulerConfig,
        },
        crypto::CredentialCipher,
        nga::NgaClient,
    };

    async fn test_state() -> AppState {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("test database must connect");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys must enable");
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .expect("migrations must run");
        AppState {
            pool,
            config: Arc::new(AppConfig {
                bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
                database_backend: DatabaseBackend::Sqlite,
                database_url: SecretString::from("postgres://unused"),
                sqlite_path: ":memory:".into(),
                database_max_connections: 1,
                api_token: SecretString::from("test-token"),
                admin_password: SecretString::from("test-password"),
                credential_encryption_key: SecretString::from(STANDARD.encode([7_u8; 32])),
                nga_user_agent: "test-agent".to_owned(),
                run_migrations: false,
                persistence: PersistenceConfig {
                    store_raw_payload: false,
                },
                assets: AssetsConfig {
                    download_enabled: false,
                    storage_path: "./data/test-assets".into(),
                    max_download_bytes: 10 * 1024 * 1024,
                },
                scheduler: SchedulerConfig {
                    default_interval_seconds: 60,
                    timezone_offset: time::UtcOffset::UTC,
                },
                observability: ObservabilityConfig {
                    log_filter: "info".to_owned(),
                    log_json: false,
                },
            }),
            credential_cipher: Arc::new(
                CredentialCipher::from_base64(&STANDARD.encode([7_u8; 32])).unwrap(),
            ),
            nga_client: NgaClient::new("test-agent".to_owned()).unwrap(),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            feishu_channel_updates: tokio::sync::watch::channel(()).0,
        }
    }

    #[test]
    fn alert_key_is_stable() {
        assert_eq!(NGA_CREDENTIALS_INVALID_KEY, "nga_credentials_invalid");
    }

    #[tokio::test]
    async fn invalid_alert_is_deduplicated_and_requeued_after_resolution() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO notification_channels
             (id, channel_type, label, config_encrypted)
             VALUES ('channel', 'bark', 'test', X'00')",
        )
        .execute(&state.pool)
        .await
        .expect("channel must be inserted");

        ensure_nga_credentials_invalid_alert(&state)
            .await
            .expect("alert must be created");
        ensure_nga_credentials_invalid_alert(&state)
            .await
            .expect("duplicate alert must be ignored");
        let alerts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_alerts")
            .fetch_one(&state.pool)
            .await
            .expect("alerts must count");
        let outbox: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM system_alert_outbox")
            .fetch_one(&state.pool)
            .await
            .expect("outbox must count");
        assert_eq!(alerts, 1);
        assert_eq!(outbox, 1);

        resolve_nga_credentials_invalid_alert(&state)
            .await
            .expect("alert must resolve");
        ensure_nga_credentials_invalid_alert(&state)
            .await
            .expect("alert must reopen");
        let row = sqlx::query(
            "SELECT o.status, CAST(a.resolved_at AS TEXT) AS resolved_at
             FROM system_alert_outbox o JOIN system_alerts a ON a.id = o.alert_id",
        )
        .fetch_one(&state.pool)
        .await
        .expect("alert row must exist");
        assert_eq!(row.get::<String, _>("status"), "pending");
        assert!(row.get::<Option<String>, _>("resolved_at").is_none());
    }
}

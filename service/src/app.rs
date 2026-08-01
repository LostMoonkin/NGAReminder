use std::{collections::HashSet, sync::Arc};

use anyhow::Context;
use axum::Router;
use secrecy::ExposeSecret;
use sqlx::{AnyPool, any::AnyPoolOptions};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, watch};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{
    api,
    config::{AppConfig, DatabaseBackend},
    crypto::CredentialCipher,
    nga::NgaClient,
};

static POSTGRES_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/postgres");
static SQLITE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/sqlite");

#[derive(Clone)]
pub struct AppState {
    pub pool: AnyPool,
    pub config: Arc<AppConfig>,
    pub credential_cipher: Arc<CredentialCipher>,
    pub nga_client: NgaClient,
    pub admin_sessions: Arc<RwLock<HashSet<String>>>,
    pub feishu_channel_updates: watch::Sender<()>,
}

pub struct Application {
    state: AppState,
    router: Router,
}

impl Application {
    pub async fn build(config: Arc<AppConfig>) -> anyhow::Result<Self> {
        sqlx::any::install_default_drivers();
        let database_url = match config.database_backend {
            DatabaseBackend::Postgres => config.database_url.expose_secret().to_owned(),
            DatabaseBackend::Sqlite => {
                if let Some(parent) = config.sqlite_path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!(
                            "failed to create SQLite parent directory {}",
                            parent.display()
                        )
                    })?;
                }
                format!("sqlite://{}?mode=rwc", config.sqlite_path.display())
            }
        };
        let pool = AnyPoolOptions::new()
            .max_connections(config.database_max_connections)
            .connect(&database_url)
            .await
            .with_context(|| format!("failed to connect to {:?}", config.database_backend))?;

        if config.database_backend == DatabaseBackend::Sqlite {
            for statement in [
                "PRAGMA journal_mode = WAL",
                "PRAGMA busy_timeout = 5000",
                "PRAGMA foreign_keys = ON",
            ] {
                sqlx::query(statement)
                    .execute(&pool)
                    .await
                    .with_context(|| format!("failed to configure SQLite: {statement}"))?;
            }
        }

        if config.run_migrations {
            let migrator = match config.database_backend {
                DatabaseBackend::Postgres => &POSTGRES_MIGRATOR,
                DatabaseBackend::Sqlite => &SQLITE_MIGRATOR,
            };
            migrator
                .run(&pool)
                .await
                .context("failed to run database migrations")?;
        }

        let credential_cipher = Arc::new(
            CredentialCipher::from_base64(config.credential_encryption_key.expose_secret())
                .context("invalid credential encryption key")?,
        );
        let nga_client = NgaClient::new(config.nga_user_agent.clone())
            .context("failed to create NGA HTTP client")?;
        let state = AppState {
            pool,
            config,
            credential_cipher,
            nga_client,
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            feishu_channel_updates: watch::channel(()).0,
        };
        let router = api::router(state.clone());

        Ok(Self { state, router })
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub async fn run_http(&self, cancellation: CancellationToken) -> anyhow::Result<()> {
        let listener = TcpListener::bind(self.state.config.bind_addr)
            .await
            .with_context(|| {
                format!(
                    "failed to bind HTTP listener to {}",
                    self.state.config.bind_addr
                )
            })?;

        info!(address = %self.state.config.bind_addr, "HTTP server listening");

        axum::serve(listener, self.router.clone())
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .await
            .context("HTTP server failed")
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;

    use super::Application;
    use crate::config::{
        AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
        SchedulerConfig,
    };

    #[tokio::test]
    async fn sqlite_creates_parent_file_and_runs_migrations() {
        let root = std::env::temp_dir().join(format!("nga-reminder-{}", uuid::Uuid::new_v4()));
        let database = root.join("nested/service.db");
        let config = Arc::new(AppConfig {
            bind_addr: "127.0.0.1:0"
                .parse::<SocketAddr>()
                .expect("test address must be valid"),
            database_backend: DatabaseBackend::Sqlite,
            database_url: SecretString::from("postgres://unused"),
            sqlite_path: database.clone(),
            database_max_connections: 1,
            api_token: SecretString::from("test-token"),
            admin_password: SecretString::from("test-password"),
            credential_encryption_key: SecretString::from(STANDARD.encode([7_u8; 32])),
            nga_user_agent: "test".to_owned(),
            run_migrations: true,
            persistence: PersistenceConfig {
                store_raw_payload: false,
            },
            assets: AssetsConfig {
                download_enabled: false,
                storage_path: root.join("assets"),
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
        });

        let application = Application::build(config)
            .await
            .expect("SQLite application must build");
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'nga_accounts'",
        )
        .fetch_one(&application.state.pool)
        .await
        .expect("migration table query must succeed");
        assert_eq!(table_count, 1);
        assert!(database.is_file());

        application.state.pool.close().await;
        std::fs::remove_dir_all(root).expect("temporary SQLite directory must be removable");
    }
}

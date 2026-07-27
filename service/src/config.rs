use std::{net::SocketAddr, path::PathBuf};

use config::{Config, Environment, File};
use secrecy::SecretString;
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub database_backend: DatabaseBackend,
    pub database_url: SecretString,
    pub sqlite_path: PathBuf,
    pub database_max_connections: u32,
    pub api_token: SecretString,
    pub admin_password: SecretString,
    pub credential_encryption_key: SecretString,
    pub nga_user_agent: String,
    pub run_migrations: bool,
    pub observability: ObservabilityConfig,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseBackend {
    Postgres,
    Sqlite,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObservabilityConfig {
    pub log_filter: String,
    pub log_json: bool,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    bind_addr: SocketAddr,
    database_backend: DatabaseBackend,
    database_url: String,
    sqlite_path: PathBuf,
    database_max_connections: u32,
    api_token: String,
    admin_password: String,
    credential_encryption_key: String,
    nga_user_agent: String,
    run_migrations: bool,
    observability: ObservabilityConfig,
}

impl AppConfig {
    pub fn load() -> Result<Self, config::ConfigError> {
        let raw: RawConfig = Config::builder()
            .set_default("bind_addr", "127.0.0.1:8080")?
            .set_default("database_backend", "postgres")?
            .set_default(
                "database_url",
                "postgres://nga_reminder:nga_reminder@127.0.0.1:5432/nga_reminder",
            )?
            .set_default("sqlite_path", "./data/nga-reminder.db")?
            .set_default("database_max_connections", 10)?
            .set_default("run_migrations", true)?
            .set_default(
                "nga_user_agent",
                "Mozilla/5.0 (compatible; NGA-Reminder/0.1)",
            )?
            .set_default("observability.log_filter", "info,sqlx=warn")?
            .set_default("observability.log_json", false)?
            .add_source(File::with_name("config/default").required(false))
            .add_source(
                Environment::with_prefix("NGA_REMINDER")
                    .prefix_separator("__")
                    .separator("__"),
            )
            .build()?
            .try_deserialize()?;

        if raw.api_token.trim().is_empty() {
            return Err(config::ConfigError::Message(
                "NGA_REMINDER__API_TOKEN must not be empty".to_owned(),
            ));
        }
        if raw.admin_password.trim().is_empty() {
            return Err(config::ConfigError::Message(
                "NGA_REMINDER__ADMIN_PASSWORD must not be empty".to_owned(),
            ));
        }
        if raw.credential_encryption_key.trim().is_empty() {
            return Err(config::ConfigError::Message(
                "NGA_REMINDER__CREDENTIAL_ENCRYPTION_KEY must not be empty".to_owned(),
            ));
        }
        if raw.database_backend == DatabaseBackend::Postgres && raw.database_url.trim().is_empty() {
            return Err(config::ConfigError::Message(
                "NGA_REMINDER__DATABASE_URL must not be empty for PostgreSQL".to_owned(),
            ));
        }
        if raw.database_backend == DatabaseBackend::Sqlite && raw.sqlite_path.as_os_str().is_empty()
        {
            return Err(config::ConfigError::Message(
                "NGA_REMINDER__SQLITE_PATH must not be empty for SQLite".to_owned(),
            ));
        }

        Ok(Self {
            bind_addr: raw.bind_addr,
            database_backend: raw.database_backend,
            database_url: SecretString::from(raw.database_url),
            sqlite_path: raw.sqlite_path,
            database_max_connections: raw.database_max_connections,
            api_token: SecretString::from(raw.api_token),
            admin_password: SecretString::from(raw.admin_password),
            credential_encryption_key: SecretString::from(raw.credential_encryption_key),
            nga_user_agent: raw.nga_user_agent,
            run_migrations: raw.run_migrations,
            observability: raw.observability,
        })
    }
}

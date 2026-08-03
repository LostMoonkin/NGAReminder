use std::{net::SocketAddr, path::PathBuf};

use config::{Config, Environment, File};
use secrecy::SecretString;
use serde::Deserialize;
use time::UtcOffset;

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
    pub persistence: PersistenceConfig,
    pub assets: AssetsConfig,
    pub scheduler: SchedulerConfig,
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

#[derive(Clone, Debug, Deserialize)]
pub struct PersistenceConfig {
    pub store_raw_payload: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AssetsConfig {
    pub download_enabled: bool,
    pub storage_path: PathBuf,
    pub max_download_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub default_interval_seconds: i32,
    pub timezone_offset: UtcOffset,
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
    persistence: PersistenceConfig,
    assets: AssetsConfig,
    scheduler: RawSchedulerConfig,
    observability: ObservabilityConfig,
}

#[derive(Debug, Deserialize)]
struct RawSchedulerConfig {
    default_interval_seconds: i32,
    timezone_offset: String,
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
            .set_default("persistence.store_raw_payload", false)?
            .set_default("assets.download_enabled", false)?
            .set_default("assets.storage_path", "./data/assets")?
            .set_default("assets.max_download_bytes", 10485760_u64)?
            .set_default("scheduler.default_interval_seconds", 60)?
            .set_default("scheduler.timezone_offset", "+08:00")?
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
        if !crate::schedule::validate_interval(raw.scheduler.default_interval_seconds) {
            return Err(config::ConfigError::Message(
                "scheduler.default_interval_seconds must be between 30 and 86400".to_owned(),
            ));
        }
        let timezone_offset = parse_timezone_offset(&raw.scheduler.timezone_offset)?;
        if raw.assets.max_download_bytes == 0 {
            return Err(config::ConfigError::Message(
                "assets.max_download_bytes must be greater than zero".to_owned(),
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
            persistence: raw.persistence,
            assets: raw.assets,
            scheduler: SchedulerConfig {
                default_interval_seconds: raw.scheduler.default_interval_seconds,
                timezone_offset,
            },
            observability: raw.observability,
        })
    }
}

fn parse_timezone_offset(value: &str) -> Result<UtcOffset, config::ConfigError> {
    if value == "Z" || value == "+00:00" {
        return Ok(UtcOffset::UTC);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 6 || (bytes[0] != b'+' && bytes[0] != b'-') || bytes[3] != b':' {
        return Err(config::ConfigError::Message(
            "scheduler.timezone_offset must use +HH:MM format".to_owned(),
        ));
    }
    let hours = value[1..3].parse::<i32>().map_err(|_| {
        config::ConfigError::Message("invalid scheduler.timezone_offset".to_owned())
    })?;
    let minutes = value[4..6].parse::<i32>().map_err(|_| {
        config::ConfigError::Message("invalid scheduler.timezone_offset".to_owned())
    })?;
    let seconds = (hours * 3_600 + minutes * 60) * if bytes[0] == b'-' { -1 } else { 1 };
    UtcOffset::from_whole_seconds(seconds).map_err(|_| {
        config::ConfigError::Message("scheduler.timezone_offset is out of range".to_owned())
    })
}

mod account;
mod admin;
mod auth;
mod export;
mod health;
mod notification;
mod query;
mod watch;

use axum::{
    Json, Router, middleware,
    routing::{delete, get, patch, post, put},
};
use serde::Serialize;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::app::AppState;
use crate::metrics;

pub fn router(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .route("/metrics", get(metrics::endpoint))
        .route("/admin", get(admin::page))
        .route("/admin/login", post(admin::login));
    let public = public.route("/admin/logout", post(admin::logout));

    let protected = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/overview", get(query::overview))
        .route("/api/v1/settings/nga-account", get(account::get))
        .route("/api/v1/settings/nga-account", put(account::save))
        .route("/api/v1/settings/nga-account/test", post(account::test))
        .route("/api/v1/watches", get(watch::list))
        .route("/api/v1/watches/threads", post(watch::create_thread))
        .route("/api/v1/watches/users", post(watch::create_user))
        .route("/api/v1/watches/{id}", patch(watch::update))
        .route("/api/v1/watches/{id}", delete(watch::delete))
        .route("/api/v1/watches/{id}/run", post(watch::run))
        .route("/api/v1/threads", get(query::threads))
        .route("/api/v1/threads/{tid}/posts", get(query::posts))
        .route("/api/v1/channels", get(notification::list_channels))
        .route("/api/v1/channels", post(notification::create_channel))
        .route("/api/v1/channels/{id}", patch(notification::update_channel))
        .route(
            "/api/v1/channels/{id}",
            delete(notification::delete_channel),
        )
        .route(
            "/api/v1/channels/{id}/test",
            post(notification::test_channel),
        )
        .route("/api/v1/notification-rules", get(notification::list_rules))
        .route(
            "/api/v1/notification-rules",
            post(notification::create_rule),
        )
        .route(
            "/api/v1/notification-rules/{id}",
            patch(notification::update_rule),
        )
        .route(
            "/api/v1/notification-rules/{id}",
            delete(notification::delete_rule),
        )
        .route("/api/v1/events", get(query::events))
        .route("/api/v1/events/{id}/read", post(query::mark_event_read))
        .route("/api/v1/events/read-all", post(query::mark_all_events_read))
        .route("/api/v1/exports/threads/{tid}", get(export::thread))
        .route("/api/v1/exports/users/{uid}", get(export::user))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_token,
        ));

    public
        .merge(protected)
        .with_state(state)
        .layer(middleware::from_fn(metrics::record_http))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

#[derive(Serialize)]
struct StatusResponse {
    service: &'static str,
    version: &'static str,
}

async fn status() -> Json<StatusResponse> {
    Json(StatusResponse {
        service: "nga-reminder",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, sync::Arc};

    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use sqlx::any::AnyPoolOptions;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use super::router;
    use crate::{
        app::AppState,
        config::{
            AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
            SchedulerConfig,
        },
        crypto::CredentialCipher,
        nga::NgaClient,
    };

    fn test_state() -> AppState {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .expect("test database URL must be valid");
        let config = AppConfig {
            bind_addr: "127.0.0.1:0"
                .parse::<SocketAddr>()
                .expect("test socket address must be valid"),
            database_backend: DatabaseBackend::Sqlite,
            database_url: SecretString::from("postgres://redacted"),
            sqlite_path: ":memory:".into(),
            database_max_connections: 1,
            api_token: SecretString::from("test-token"),
            admin_password: SecretString::from("test-password"),
            credential_encryption_key: SecretString::from(STANDARD.encode([7_u8; 32])),
            nga_user_agent: "test".to_owned(),
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
        };

        AppState {
            pool,
            config: Arc::new(config),
            credential_cipher: Arc::new(
                CredentialCipher::from_base64(&STANDARD.encode([7_u8; 32]))
                    .expect("test cipher must build"),
            ),
            nga_client: NgaClient::new("test".to_owned()).expect("test client must build"),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    #[tokio::test]
    async fn health_is_public() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_is_public() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
    }

    #[tokio::test]
    async fn protected_route_requires_token() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn protected_route_accepts_token() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .header("authorization", "Bearer test-token")
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_login_establishes_session() {
        let app = router(test_state());
        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"password":"test-password"}"#))
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login
            .headers()
            .get(header::SET_COOKIE)
            .expect("login must set a cookie")
            .to_str()
            .expect("cookie must be text")
            .split(';')
            .next()
            .expect("cookie must contain a value")
            .to_owned();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .expect("request must build"),
            )
            .await
            .expect("router must respond");

        assert_eq!(response.status(), StatusCode::OK);
    }
}

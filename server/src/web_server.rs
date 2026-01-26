use axum::{extract::State, http::StatusCode, response::Json, routing::post, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::config_holder::ConfigHolder;

pub type SharedConfigHolder = Arc<ConfigHolder>;

#[derive(Debug, Deserialize)]
pub struct UpdatePassportRequest {
    pub cid: String,
    pub uid: String,
}

#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn update_passport_handler(
    State(config_holder): State<SharedConfigHolder>,
    Json(payload): Json<UpdatePassportRequest>,
) -> Result<Json<ApiResponse>, (StatusCode, Json<ApiResponse>)> {
    match config_holder.update_passport(payload.cid, payload.uid).await {
        Ok(_) => Ok(Json(ApiResponse {
            success: true,
            message: Some("Passport updated successfully".to_string()),
            error: None,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                success: false,
                message: None,
                error: Some(format!("Failed to update passport: {}", e)),
            }),
        )),
    }
}

pub fn create_router(config_holder: SharedConfigHolder) -> Router {
    Router::new()
        .route("/api/passport", post(update_passport_handler))
        .layer(CorsLayer::permissive())
        .with_state(config_holder)
}

pub async fn run_server(config_holder: SharedConfigHolder) {
    let web_config = config_holder.get_web_config().await;
    let app = create_router(config_holder);
    let addr = format!("{}:{}", web_config.host, web_config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind server");

    println!("Web server listening on {}", addr);

    axum::serve(listener, app)
        .await
        .expect("Failed to start server");
}

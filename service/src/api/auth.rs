use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use secrecy::ExposeSecret;
use serde::Serialize;
use subtle::ConstantTimeEq;

use crate::app::AppState;

#[derive(Serialize)]
pub(super) struct ErrorResponse {
    pub(super) error: &'static str,
}

pub(super) async fn require_api_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let supplied_bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    let expected = state.config.api_token.expose_secret().as_bytes();
    let bearer_authorized = supplied_bearer
        .map(str::as_bytes)
        .filter(|value| value.len() == expected.len())
        .is_some_and(|value| bool::from(value.ct_eq(expected)));
    let supplied_session = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix("nga_reminder_session=")
                    .map(str::to_owned)
            })
        });
    let session_authorized = if let Some(session) = supplied_session {
        state.admin_sessions.read().await.contains(&session)
    } else {
        false
    };

    if !bearer_authorized && !session_authorized {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "unauthorized",
            }),
        ));
    }

    Ok(next.run(request).await)
}

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;

use super::auth::ErrorResponse;

const ADMIN_PAGE: &str = include_str!("admin.html");

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    authenticated: bool,
}

pub async fn page() -> Html<&'static str> {
    Html(ADMIN_PAGE)
}

pub async fn login(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let expected = state.config.admin_password.expose_secret().as_bytes();
    let supplied = request.password.as_bytes();
    let valid = supplied.len() == expected.len() && bool::from(supplied.ct_eq(expected));
    if !valid {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "invalid_credentials",
            }),
        ));
    }

    let session = Uuid::new_v4().to_string();
    state.admin_sessions.write().await.insert(session.clone());
    let secure = request_headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"));
    let secure_attribute = if secure { "; Secure" } else { "" };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "nga_reminder_session={session}; Path=/; HttpOnly; SameSite=Strict{secure_attribute}"
        ))
        .expect("UUID session cookie must be a valid header"),
    );

    Ok((
        headers,
        Json(LoginResponse {
            authenticated: true,
        }),
    )
        .into_response())
}

pub async fn logout(State(state): State<AppState>, request: Request) -> StatusCode {
    let session = request
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
    if let Some(session) = session {
        state.admin_sessions.write().await.remove(&session);
    }
    StatusCode::NO_CONTENT
}

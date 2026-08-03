use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use serde::Deserialize;

use crate::{
    app::AppState,
    export::{self, ExportFormat},
};

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    format: Option<String>,
}

pub async fn thread(
    State(state): State<AppState>,
    Path(tid): Path<i64>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, (StatusCode, &'static str)> {
    build_response(
        export::thread(
            &state.pool,
            tid,
            parse_format(&query)?,
            &state.config.assets,
        )
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "export_target_not_found"))?,
    )
}

pub async fn user(
    State(state): State<AppState>,
    Path(uid): Path<i64>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, (StatusCode, &'static str)> {
    build_response(
        export::user(
            &state.pool,
            uid,
            parse_format(&query)?,
            &state.config.assets,
        )
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, "export_target_not_found"))?,
    )
}

fn parse_format(query: &ExportQuery) -> Result<ExportFormat, (StatusCode, &'static str)> {
    ExportFormat::parse(query.format.as_deref())
        .ok_or((StatusCode::BAD_REQUEST, "unsupported_export_format"))
}

fn build_response(
    artifact: export::ExportArtifact,
) -> Result<Response, (StatusCode, &'static str)> {
    let mut response = Response::new(artifact.body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(artifact.content_type),
    );
    let disposition = format!("attachment; filename=\"{}\"", artifact.filename);
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition)
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "invalid_export_filename"))?,
    );
    if let Some(content_length) = artifact.content_length {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&content_length.to_string())
                .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "invalid_export_size"))?,
        );
    }
    Ok(response)
}

fn internal(_: anyhow::Error) -> (StatusCode, &'static str) {
    (StatusCode::INTERNAL_SERVER_ERROR, "export_failed")
}

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use solver_core::{NumericOptions, SolveOptions};
use tracing::instrument;
use uuid::Uuid;

use crate::{
    db::{AppState, ensure_prepared, fetch_snapshot_sparse_data},
    types::{PrepareHttpBody, SolveHttpBody},
};

/// Query parameters for status endpoint.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct StatusQuery {
    /// Optional print level used in factorization key.
    pub print_level: Option<f64>,
}

/// Builds internal API router.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // Preferred snapshot-oriented API paths.
        .route("/internal/snapshots/{snapshot_id}/prepare", post(prepare))
        .route(
            "/internal/snapshots/{snapshot_id}/factorization",
            get(factorization_status),
        )
        .route("/internal/snapshots/{snapshot_id}/solve", post(solve))
        .route(
            "/internal/snapshots/{snapshot_id}/invalidate",
            post(invalidate),
        )
        // Backward-compatible model-oriented aliases.
        .route("/internal/models/{snapshot_id}/prepare", post(prepare))
        .route(
            "/internal/models/{snapshot_id}/factorization",
            get(factorization_status),
        )
        .route("/internal/models/{snapshot_id}/solve", post(solve))
        .route(
            "/internal/models/{snapshot_id}/invalidate",
            post(invalidate),
        )
        .with_state(state)
}

#[instrument(skip(state))]
async fn prepare(
    State(state): State<Arc<AppState>>,
    Path(snapshot_id): Path<Uuid>,
    Json(body): Json<PrepareHttpBody>,
) -> impl IntoResponse {
    match fetch_snapshot_sparse_data(&state, snapshot_id).await {
        Ok(data) => match state.solver.prepare(
            &data,
            NumericOptions {
                print_level: body.print_level.unwrap_or(0.0),
            },
        ) {
            Ok(result) => (
                StatusCode::OK,
                Json(serde_json::to_value(result).unwrap_or_default()),
            )
                .into_response(),
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response(),
        },
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

#[instrument(skip(state))]
async fn factorization_status(
    State(state): State<Arc<AppState>>,
    Path(snapshot_id): Path<Uuid>,
    Query(query): Query<StatusQuery>,
) -> impl IntoResponse {
    let print_level = query.print_level.unwrap_or(0.0);
    let status = state
        .solver
        .factorization_status(snapshot_id, NumericOptions { print_level });

    (StatusCode::OK, Json(serde_json::json!({"status": status}))).into_response()
}

#[instrument(skip(state))]
async fn solve(
    State(state): State<Arc<AppState>>,
    Path(snapshot_id): Path<Uuid>,
    Json(body): Json<SolveHttpBody>,
) -> impl IntoResponse {
    let print_level = body.print_level.unwrap_or(0.0);
    if let Err(err) = ensure_prepared(&state, snapshot_id, print_level).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response();
    }

    let options = SolveOptions {
        return_x: body.solve.return_x,
        return_g: body.solve.return_g,
        return_h: body.solve.return_h,
    };

    if let Some(rhs) = body.rhs {
        return match state.solver.solve_one(
            snapshot_id,
            NumericOptions { print_level },
            &rhs,
            options,
        ) {
            Ok(result) => (
                StatusCode::OK,
                Json(serde_json::to_value(result).unwrap_or_default()),
            )
                .into_response(),
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response(),
        };
    }

    if let Some(rhs_batch) = body.rhs_batch {
        return match state.solver.solve_batch(
            snapshot_id,
            NumericOptions { print_level },
            &rhs_batch,
            options,
        ) {
            Ok(result) => (
                StatusCode::OK,
                Json(serde_json::to_value(result).unwrap_or_default()),
            )
                .into_response(),
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response(),
        };
    }

    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": "missing rhs or rhs_batch"})),
    )
        .into_response()
}

#[instrument(skip(state))]
async fn invalidate(
    State(state): State<Arc<AppState>>,
    Path(snapshot_id): Path<Uuid>,
) -> impl IntoResponse {
    let invalidated = state.solver.invalidate(snapshot_id);
    (
        StatusCode::OK,
        Json(serde_json::json!({"invalidated": invalidated})),
    )
        .into_response()
}

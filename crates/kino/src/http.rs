use crate::state::ProbeStore;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prost::Message;

#[derive(Clone)]
struct AppState {
    probe_store: ProbeStore,
}

pub(crate) fn build_router(probe_store: ProbeStore) -> Router {
    let state = AppState { probe_store };

    Router::new()
        .route("/probes", get(get_probes))
        .with_state(state)
}

async fn get_probes(State(state): State<AppState>) -> Response {
    let snapshot = state.probe_store.snapshot_proto().await;

    let mut bytes = Vec::with_capacity(snapshot.encoded_len());
    if snapshot.encode(&mut bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    (
        [(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/x-protobuf"),
        )],
        bytes,
    )
        .into_response()
}

use axum::extract::State;
use axum::response::IntoResponse;

use crate::AppState;

pub(crate) async fn get(State(state): State<AppState>) -> impl IntoResponse {
    let priority_str = state.config.priority.to_string();

    let body = crate::build_bytes!(
        b"StoreDir: ",
        state.config.store.virtual_store(),
        b"\nWantMassQuery: 1\nPriority: ",
        priority_str.as_bytes(),
        b"\n"
    );

    (
        [(axum::http::header::CONTENT_TYPE, "text/x-nix-cache-info")],
        body,
    )
}

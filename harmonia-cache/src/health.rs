use axum::response::IntoResponse;

pub(crate) async fn get() -> impl IntoResponse {
    "OK\n"
}

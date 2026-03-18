use axum::response::IntoResponse;

pub(crate) async fn get() -> impl IntoResponse {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

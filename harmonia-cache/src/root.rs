use std::collections::HashMap;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};

use crate::TAILWIND_CSS;
use crate::template::{LANDING_TEMPLATE, LANDING_WITH_KEYS_TEMPLATE, render, render_page};
use crate::{AppState, CARGO_HOME_PAGE, CARGO_NAME, CARGO_VERSION};

pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, std::convert::Infallible> {
    let mut vars = HashMap::new();
    vars.insert("version", CARGO_VERSION.to_string());
    vars.insert(
        "store",
        String::from_utf8_lossy(state.config.store.virtual_store()).to_string(),
    );
    vars.insert("priority", state.config.priority.to_string());
    vars.insert("homepage", CARGO_HOME_PAGE.to_string());
    vars.insert("name", CARGO_NAME.to_string());

    // Determine scheme: check X-Forwarded-Proto first, default to https
    let scheme = headers
        .get("X-Forwarded-Proto")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https".to_string());

    // Get cache URL from Host header
    let host = headers
        .get("Host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("cache.example.com");
    let cache_url = format!("{scheme}://{host}");
    vars.insert("cache_url", cache_url);

    // Get public keys from configured signing keys
    let public_keys: Vec<String> = state
        .config
        .secret_keys
        .iter()
        .map(|sk| sk.to_public_key().to_string())
        .collect();

    // Choose template based on whether keys are configured
    let template = if public_keys.is_empty() {
        LANDING_TEMPLATE
    } else {
        // Space-separated keys for CLI/nix.conf usage
        vars.insert("public_keys_cli", public_keys.join(" "));
        // Quoted keys for Nix list literals
        vars.insert(
            "public_keys_list",
            public_keys
                .iter()
                .map(|k| format!("\"{k}\""))
                .collect::<Vec<_>>()
                .join(" "),
        );
        LANDING_WITH_KEYS_TEMPLATE
    };

    let content = render(template, vars);
    let html = render_page(
        &format!("Nix Binary Cache - {CARGO_NAME} {CARGO_VERSION}"),
        TAILWIND_CSS,
        &content,
    );

    Ok(Html(html).into_response())
}

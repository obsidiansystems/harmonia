#![warn(clippy::dbg_macro)]

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use config::Config;
use error::{CacheError, IoErrorContext, Result, StoreError};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use url::Url;

use harmonia_store_core::store_path::{StorePath, StorePathHash};
use harmonia_store_remote::DaemonStore;

/// Macro for building byte vectors efficiently from parts
#[macro_export]
macro_rules! build_bytes {
    ($($part:expr),* $(,)?) => {{
        let parts: &[&[u8]] = &[$($part),*];
        let capacity = parts.iter().map(|p| p.len()).sum();
        let mut result = Vec::with_capacity(capacity);
        for part in parts {
            result.extend_from_slice(part);
        }
        result
    }};
}

mod buildlog;
mod cacheinfo;
mod config;
mod error;
mod health;
mod nar;
mod narinfo;
mod narlist;
mod prometheus;
mod root;
mod serve;
mod store;
mod template;
mod tls;
mod version;

#[derive(Clone)]
pub(crate) struct AppState {
    config: Arc<Config>,
    metrics: Arc<prometheus::PrometheusMetrics>,
}

async fn nixhash(state: &AppState, hash: &[u8]) -> Result<Option<StorePath>> {
    // Parse the hash bytes into a StorePathHash
    let store_path_hash =
        StorePathHash::decode_digest(hash).map_err(|e| StoreError::PathQuery {
            hash: String::from_utf8_lossy(hash).to_string(),
            reason: format!("Invalid hash format: {e}"),
        })?;

    let mut guard = state.config.store.acquire().await?;

    guard
        .client()
        .query_path_from_hash_part(&store_path_hash)
        .await
        .map_err(|e| {
            CacheError::from(StoreError::PathQuery {
                hash: String::from_utf8_lossy(hash).to_string(),
                reason: e.to_string(),
            })
        })
}

const TAILWIND_CSS: &str = include_str!("styles/output.css");

const CARGO_NAME: &str = env!("CARGO_PKG_NAME");
const CARGO_VERSION: &str = env!("CARGO_PKG_VERSION");
const CARGO_HOME_PAGE: &str = env!("CARGO_PKG_HOMEPAGE");
const NIXBASE32_ALPHABET: &str = "0123456789abcdfghijklmnpqrsvwxyz";

fn cache_control_max_age(max_age: u32) -> String {
    format!("max-age={max_age}")
}

fn cache_control_max_age_1y() -> String {
    cache_control_max_age(365 * 24 * 60 * 60)
}

fn cache_control_max_age_1d() -> String {
    cache_control_max_age(24 * 60 * 60)
}

fn cache_control_no_store() -> &'static str {
    "no-store"
}

macro_rules! some_or_404 {
    ($res:expr) => {
        match $res {
            Some(val) => val,
            None => {
                return Ok((
                    StatusCode::NOT_FOUND,
                    [(
                        axum::http::header::CACHE_CONTROL,
                        crate::cache_control_no_store(),
                    )],
                    "missed hash",
                )
                    .into_response())
            }
        }
    };
}
pub(crate) use some_or_404;

#[derive(Debug)]
struct ServerError {
    err: CacheError,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.err)
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = match &self.err {
            CacheError::Store(StoreError::PathQuery { .. }) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.err.to_string()).into_response()
    }
}

impl From<CacheError> for ServerError {
    fn from(err: CacheError) -> ServerError {
        ServerError { err }
    }
}

type ServerResult = std::result::Result<Response, ServerError>;

/// Dispatch handler for `/:file` — routes `{hash}.narinfo` and `{hash}.ls`
async fn dotfile_dispatch(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
    req: axum::extract::Request,
) -> ServerResult {
    if let Some(hash) = file.strip_suffix(".narinfo") {
        let uri = req.uri().clone();
        let query = uri.query().map(|q| q.to_string());
        narinfo::get(State(state), axum::extract::Path(hash.to_string()), query).await
    } else if let Some(hash) = file.strip_suffix(".ls") {
        narlist::get(State(state), axum::extract::Path(hash.to_string())).await
    } else {
        Ok((StatusCode::NOT_FOUND, "not found").into_response())
    }
}

async fn inner_main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (metrics, pool_metrics) = prometheus::initialize_metrics()?;
    let config = config::load(Some(pool_metrics))?;

    let bind = config.bind.clone();
    let enable_compression = config.enable_compression;
    let tls_cert_path = config.tls_cert_path.clone();
    let tls_key_path = config.tls_key_path.clone();

    let state = AppState {
        config: Arc::new(config),
        metrics,
    };

    let router = Router::new()
        .route("/", get(root::get))
        .route("/{file}", get(dotfile_dispatch).head(dotfile_dispatch))
        .route("/nar/{file}", get(nar::get))
        .route("/serve/{hash}", get(serve::get_root))
        .route("/serve/{hash}/", get(serve::get_root))
        .route("/serve/{hash}/{*path}", get(serve::get))
        .route("/log/{drv}", get(buildlog::get))
        .route("/version", get(version::get))
        .route("/health", get(health::get))
        .route("/nix-cache-info", get(cacheinfo::get))
        .route("/metrics", get(prometheus::metrics_handler))
        .with_state(state.clone());

    let router = router.layer(prometheus::PrometheusLayer::new(state.metrics.clone()));

    let app = if enable_compression {
        router.layer(tower_http::compression::CompressionLayer::new().zstd(true))
    } else {
        router
    };

    log::info!("listening on {}", bind);

    let try_url = Url::parse(&bind);
    let (bind_addr, uds) = if let Ok(url) = try_url.as_ref() {
        if url.scheme() != "unix" {
            (bind.as_str(), false)
        } else if url.host().is_none() {
            (url.path(), true)
        } else {
            return Err(error::ServerError::Startup {
                reason: "Can only bind to file URLs without host portion.".to_string(),
            }
            .into());
        }
    } else {
        (bind.as_str(), false)
    };

    if tls_cert_path.is_some() || tls_key_path.is_some() {
        if uds {
            log::error!("TLS is not supported with Unix domain sockets.");
            std::process::exit(1);
        }
        let tls_config = tls::load_tls_config(
            Path::new(
                tls_cert_path
                    .as_ref()
                    .expect("tls certificate path must be set when tls is enabled"),
            ),
            Path::new(
                tls_key_path
                    .as_ref()
                    .expect("tls key path must be set when tls is enabled"),
            ),
        )?;

        let rustls_config =
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config));
        let addr: std::net::SocketAddr =
            bind_addr.parse().map_err(|e| error::ServerError::Startup {
                reason: format!("Invalid bind address: {e}"),
            })?;
        axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await
            .io_context("Failed to start TLS server")?;
    } else if uds {
        if !cfg!(unix) {
            log::error!("Binding to Unix domain sockets is only supported on Unix.");
            std::process::exit(1);
        } else {
            let socket_path = Path::new(bind_addr);
            // Remove existing socket file if present
            if socket_path.exists() {
                fs::remove_file(socket_path).io_context("Failed to remove existing socket file")?;
            }
            let listener = tokio::net::UnixListener::bind(socket_path)
                .io_context("Failed to bind to Unix domain socket")?;
            fs::set_permissions(socket_path, fs::Permissions::from_mode(0o777))
                .io_context("Failed to set socket permissions")?;
            axum::serve(listener, app)
                .await
                .io_context("Failed to start UDS server")?;
        }
    } else {
        let listener = tokio::net::TcpListener::bind(bind_addr)
            .await
            .io_context("Failed to bind server")?;
        axum::serve(listener, app)
            .await
            .io_context("Failed to start server")?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    inner_main().await.map_err(std::io::Error::other)
}

use crate::error::{BuildLogError, CacheError, IoErrorContext, Result};
use async_compression::tokio::bufread::BzDecoder;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use harmonia_store_core::store_path::StorePath;
use harmonia_store_remote::DaemonStore;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use tokio::io::BufReader;
use tokio_util::io::ReaderStream;

use crate::{AppState, cache_control_max_age_1y, cache_control_no_store, nixhash, some_or_404};

async fn query_drv_path(state: &AppState, drv: &[u8]) -> Result<Option<StorePath>> {
    nixhash(state, if drv.len() > 32 { &drv[0..32] } else { drv }).await
}

pub fn get_build_log(store: &Path, drv_path: &StorePath) -> Option<PathBuf> {
    // StorePath is now just "hash-name", use it directly
    let drv_name = drv_path.to_string();
    let drv_name_bytes = drv_name.as_bytes();
    let log_path = store.parent().map(|p| {
        p.join("var")
            .join("log")
            .join("nix")
            .join("drvs")
            .join(OsStr::from_bytes(&drv_name_bytes[0..2]))
            .join(OsStr::from_bytes(&drv_name_bytes[2..]))
    })?;
    if log_path.exists() {
        return Some(log_path);
    }
    // check if compressed log exists
    let log_path = log_path.with_extension("drv.bz2");
    if log_path.exists() {
        Some(log_path)
    } else {
        None
    }
}

pub(crate) async fn get(
    State(state): State<AppState>,
    axum::extract::Path(drv): axum::extract::Path<String>,
    headers: HeaderMap,
) -> crate::ServerResult {
    let drv_path =
        some_or_404!(query_drv_path(&state, drv.as_bytes()).await.map_err(
            |e| CacheError::from(BuildLogError::QueryFailed {
                reason: format!("Could not query nar hash in database for {drv}: {e}"),
            })
        )?);
    let mut guard = state.config.store.acquire().await?;

    match guard.client().is_valid_path(&drv_path).await {
        Ok(true) => (),
        Ok(false) => {
            return Ok((
                StatusCode::NOT_FOUND,
                [(header::CACHE_CONTROL, cache_control_no_store())],
            )
                .into_response());
        }
        Err(e) => {
            return Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CACHE_CONTROL, cache_control_no_store())],
                format!("Failed to query path info: {e}"),
            )
                .into_response());
        }
    }
    let build_log = some_or_404!(get_build_log(state.config.store.real_store(), &drv_path));
    let ext = match build_log.extension() {
        Some(ext) => ext,
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                [(header::CACHE_CONTROL, cache_control_no_store())],
            )
                .into_response());
        }
    };
    let accept_encoding = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if ext == "bz2" && !accept_encoding.contains("bzip2") {
        // Decompress the bz2 file and serve the decompressed content
        let file = tokio::fs::File::open(&build_log)
            .await
            .io_context(format!("Failed to open build log: {}", build_log.display()))?;
        let reader = BufReader::new(file);
        let decompressed_stream = BzDecoder::new(reader);
        let stream = ReaderStream::new(decompressed_stream);

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CACHE_CONTROL, cache_control_max_age_1y())
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from_stream(stream))
            .unwrap()
            .into_response());
    }

    // Serve the file as-is with the appropriate Content-Encoding header
    let file = tokio::fs::File::open(&build_log)
        .await
        .io_context(format!("Failed to open build log: {}", build_log.display()))?;
    let metadata = file.metadata().await.io_context(format!(
        "Failed to read build log metadata: {}",
        build_log.display()
    ))?;
    let stream = ReaderStream::new(file);

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, cache_control_max_age_1y())
        .header(header::CONTENT_LENGTH, metadata.len().to_string());

    if ext == "bz2" {
        builder = builder
            .header(header::CONTENT_ENCODING, "bzip2")
            .header(header::CONTENT_TYPE, "application/octet-stream");
    } else {
        builder = builder.header(header::CONTENT_TYPE, "text/plain; charset=utf-8");
        if state.config.enable_compression {
            // don't allow compression middleware to modify partial content
            builder = builder.header(header::CONTENT_ENCODING, "none");
        }
    }

    Ok(builder
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response())
}

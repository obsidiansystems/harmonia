use std::path::{Path, PathBuf};

use crate::error::IoErrorContext;
use askama_escape::{Html, escape as escape_html_entity};
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::{CONTROLS, utf8_percent_encode};
use tokio_util::io::ReaderStream;

use crate::template::{DIRECTORY_ROW_TEMPLATE, DIRECTORY_TEMPLATE, render, render_page};
use crate::{
    AppState, CARGO_NAME, CARGO_VERSION, ServerResult, TAILWIND_CSS, nixhash, some_or_404,
};

/// Returns percent encoded file URL path.
macro_rules! encode_file_url {
    ($path:ident) => {
        utf8_percent_encode(&$path, CONTROLS)
    };
}

/// Returns HTML entity encoded formatter.
///
/// ```plain
/// " => &quot;
/// & => &amp;
/// ' => &#x27;
/// < => &lt;
/// > => &gt;
/// / => &#x2f;
/// ```
macro_rules! encode_file_name {
    ($entry:ident) => {
        escape_html_entity(&$entry.file_name().to_string_lossy(), Html)
    };
}

// human readable file size
fn file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.2} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GiB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    }
}

pub(crate) fn directory_listing(
    url_prefix: &Path,
    fs_path: &Path,
    real_store: &Path,
) -> ServerResult {
    let path_without_store = fs_path.strip_prefix(real_store).unwrap_or(fs_path);
    let index_of = format!(
        "Index of {}",
        escape_html_entity(&path_without_store.to_string_lossy(), Html)
    );
    let mut rows = String::new();

    for entry in fs_path
        .read_dir()
        .io_context(format!("cannot read directory: {}", fs_path.display()))?
    {
        let entry = entry.io_context(format!(
            "failed to read directory entry in: {}",
            fs_path.display()
        ))?;
        let p = match entry.path().strip_prefix(fs_path) {
            Ok(p) => url_prefix.join(p).to_string_lossy().into_owned(),
            Err(_) => continue,
        };

        // if file is a directory, add '/' to the end of the name
        if let Ok(metadata) = entry.metadata() {
            let mut row_vars = std::collections::HashMap::new();
            row_vars.insert("url", encode_file_url!(p).to_string());

            if metadata.is_dir() {
                row_vars.insert("name", format!("{}/", encode_file_name!(entry)));
                row_vars.insert("size", "-".to_string());
            } else {
                row_vars.insert("name", encode_file_name!(entry).to_string());
                row_vars.insert("size", file_size(metadata.len()));
            }

            rows.push_str(&render(DIRECTORY_ROW_TEMPLATE, row_vars));
        } else {
            continue;
        }
    }

    let mut vars = std::collections::HashMap::new();
    vars.insert("index_of", index_of);
    vars.insert("rows", rows);

    let content = render(DIRECTORY_TEMPLATE, vars);
    let html = render_page(
        &format!("Nix binary cache ({CARGO_NAME} {CARGO_VERSION})"),
        TAILWIND_CSS,
        &content,
    );

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response())
}

fn guess_mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain",
        Some("xml") => "application/xml",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub(crate) async fn get_root(
    State(state): State<AppState>,
    axum::extract::Path(hash): axum::extract::Path<String>,
) -> ServerResult {
    get_inner(state, &hash, "").await
}

pub(crate) async fn get(
    State(state): State<AppState>,
    axum::extract::Path((hash, path)): axum::extract::Path<(String, String)>,
) -> ServerResult {
    get_inner(state, &hash, &path).await
}

async fn get_inner(state: AppState, hash: &str, sub_path: &str) -> ServerResult {
    let dir = Path::new(sub_path);
    let dir = dir.strip_prefix("/").unwrap_or(dir);

    let store_path_obj = some_or_404!(nixhash(&state, hash.as_bytes()).await?);
    let store_path = state.config.store.get_real_path(&store_path_obj);
    let full_path = if dir == Path::new("") {
        store_path.clone()
    } else {
        store_path.join(dir)
    };
    let full_path = full_path.canonicalize().io_context(format!(
        "cannot resolve nix store path: {}",
        full_path.display()
    ))?;

    let real_store = state
        .config
        .store
        .real_store()
        .canonicalize()
        .io_context(format!(
            "cannot resolve real nix store path: {}",
            state.config.store.real_store().display()
        ))?;

    if !full_path.starts_with(&real_store) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }

    if full_path.is_dir() {
        let index_file = full_path.join("index.html");
        if index_file.metadata().is_ok_and(|stat| stat.is_file()) {
            return serve_file(&index_file).await;
        }

        let url_prefix = PathBuf::from("/serve").join(hash);
        let url_prefix = if dir == Path::new("") {
            url_prefix
        } else {
            url_prefix.join(dir)
        };
        directory_listing(&url_prefix, &full_path, &real_store)
    } else {
        serve_file(&full_path).await
    }
}

async fn serve_file(path: &Path) -> ServerResult {
    let file = tokio::fs::File::open(path)
        .await
        .io_context(format!("cannot open file: {}", path.display()))?;
    let metadata = file
        .metadata()
        .await
        .io_context(format!("cannot read file metadata: {}", path.display()))?;
    let content_type = guess_mime(path);
    let stream = ReaderStream::new(file);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.len().to_string())
        .body(Body::from_stream(stream))
        .unwrap()
        .into_response())
}

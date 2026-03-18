use crate::error::{CacheError, StoreError};
use crate::{AppState, cache_control_max_age_1y};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use harmonia_nar::NarByteStream;
use harmonia_store_core::store_path::StorePathHash;
use harmonia_store_remote::DaemonStore;
use harmonia_utils_hash::fmt::CommonHash;
use http_body::SizeHint;
use serde::Deserialize;

// A body wrapper that reports a known content length to hyper,
// ensuring Content-Length header is sent instead of chunked encoding.
pin_project_lite::pin_project! {
    struct SizedBody {
        #[pin]
        inner: Body,
        size: u64,
    }
}

impl http_body::Body for SizedBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.project().inner.poll_frame(cx)
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.size)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

/// Represents the query string of a NAR URL.
#[derive(Debug, Deserialize)]
pub struct NarRequest {
    hash: Option<String>,
}

// TODO(conni2461): still missing
// - handle downloadHash/downloadSize and fileHash/fileSize after implementing compression

// Credit actix_web actix-files: https://github.com/actix/actix-web/blob/master/actix-files/src/range.rs
#[derive(Debug)]
struct HttpRange {
    start: u64,
    length: u64,
}

impl HttpRange {
    /// Parses Range HTTP header string as per RFC 2616.
    ///
    /// `header` is HTTP Range header (e.g. `bytes=bytes=0-9`).
    /// `size` is full size of response (file).
    fn parse(
        header: &str,
        size: u64,
    ) -> std::result::Result<Vec<Self>, http_range::HttpRangeParseError> {
        http_range::HttpRange::parse(header, size).map(|ranges| {
            ranges
                .iter()
                .map(|range| Self {
                    start: range.start,
                    length: range.length,
                })
                .collect()
        })
    }
}

/// Parse a NAR filename into (narhash, optional outhash).
/// Supports:
///   - `{narhash}.nar` (52-char nixbase32 hash)
///   - `{outhash}-{narhash}.nar` (32-char outhash + 52-char narhash)
fn parse_nar_filename(file: &str) -> Option<(String, Option<String>)> {
    let name = file.strip_suffix(".nar")?;

    if name.len() == 52 && name.chars().all(|c| crate::NIXBASE32_ALPHABET.contains(c)) {
        return Some((name.to_string(), None));
    }

    if name.len() == 85 {
        let (outhash, rest) = name.split_at(32);
        let narhash = rest.strip_prefix('-')?;
        if narhash.len() == 52
            && outhash
                .chars()
                .all(|c| crate::NIXBASE32_ALPHABET.contains(c))
            && narhash
                .chars()
                .all(|c| crate::NIXBASE32_ALPHABET.contains(c))
        {
            return Some((narhash.to_string(), Some(outhash.to_string())));
        }
    }

    None
}

pub(crate) async fn get(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<NarRequest>,
    headers: HeaderMap,
) -> crate::ServerResult {
    let (narhash, path_outhash) = match parse_nar_filename(&file) {
        Some(parsed) => parsed,
        None => {
            return Ok((StatusCode::NOT_FOUND, "invalid nar filename").into_response());
        }
    };

    // lookup the store path.
    // We usually extract the outhash from the query parameter.
    // However, when processing nix-serve URLs, it's present in the path
    // directly.
    let outhash = if let Some(outhash) = &q.hash {
        Some(outhash.as_str())
    } else {
        path_outhash.as_deref()
    };
    let store_path = match outhash {
        Some(outhash) => {
            // Parse outhash to StorePathHash
            let store_path_hash =
                StorePathHash::decode_digest(outhash.as_bytes()).map_err(|e| {
                    CacheError::from(StoreError::PathQuery {
                        hash: outhash.to_string(),
                        reason: format!("Invalid hash format: {e}"),
                    })
                })?;

            let mut guard = state.config.store.acquire().await?;
            guard
                .client()
                .query_path_from_hash_part(&store_path_hash)
                .await
                .map_err(|e| {
                    CacheError::from(StoreError::PathQuery {
                        hash: outhash.to_string(),
                        reason: e.to_string(),
                    })
                })?
        }
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                [(header::CACHE_CONTROL, crate::cache_control_no_store())],
                "missing outhash",
            )
                .into_response());
        }
    };
    let store_path = match store_path {
        Some(store_path) => store_path,
        None => {
            return Ok((
                StatusCode::NOT_FOUND,
                [(header::CACHE_CONTROL, crate::cache_control_no_store())],
                "store path not found",
            )
                .into_response());
        }
    };

    // lookup the path info.
    let info = {
        let mut guard = state.config.store.acquire().await?;

        match guard
            .client()
            .query_path_info(&store_path)
            .await
            .map_err(|e| CacheError::from(StoreError::Remote(e)))?
        {
            Some(info) => info,
            None => {
                return Ok((
                    StatusCode::NOT_FOUND,
                    [(header::CACHE_CONTROL, crate::cache_control_no_store())],
                    "path info not found",
                )
                    .into_response());
            }
        }
    }; // guard is dropped here

    // URL narhash is bare (no sha256: prefix), so use as_bare() for comparison
    let expected_hash = info.nar_hash.as_base32().as_bare().to_string();
    if narhash != expected_hash {
        return Ok((
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, crate::cache_control_no_store())],
            "hash mismatch detected",
        )
            .into_response());
    }

    let rlength = info.nar_size;
    let real_path = state.config.store.get_real_path(&store_path);

    // Credit actix_web actix-files: https://github.com/actix/actix-web/blob/master/actix-files/src/named.rs#L525
    if let Some(ranges) = headers.get(header::RANGE) {
        if let Ok(ranges_header) = ranges.to_str() {
            if let Ok(ranges) = HttpRange::parse(ranges_header, rlength) {
                let range_length = ranges[0].length;
                let offset = ranges[0].start;

                let stream = NarByteStream::new(real_path);
                let ranged_stream = create_range_stream(stream, offset, range_length);

                let mut builder = Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, "application/x-nix-archive")
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, range_length.to_string())
                    .header(
                        header::CONTENT_RANGE,
                        format!(
                            "bytes {}-{}/{}",
                            offset,
                            offset + range_length - 1,
                            info.nar_size
                        ),
                    )
                    .header(header::CACHE_CONTROL, cache_control_max_age_1y());

                if state.config.enable_compression {
                    // don't allow compression middleware to modify partial content
                    builder = builder.header(header::CONTENT_ENCODING, "none");
                }

                let sized = SizedBody {
                    inner: Body::from_stream(ranged_stream),
                    size: range_length,
                };
                return Ok(builder.body(sized).unwrap().into_response());
            } else {
                return Ok(Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{rlength}"))
                    .body(Body::empty())
                    .unwrap()
                    .into_response());
            };
        } else {
            return Ok(StatusCode::BAD_REQUEST.into_response());
        };
    }

    // Non-range request: stream the full NAR
    let stream = NarByteStream::new(real_path);
    let sized = SizedBody {
        inner: Body::from_stream(stream),
        size: rlength,
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-nix-archive")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, cache_control_max_age_1y())
        .body(sized)
        .unwrap()
        .into_response())
}

/// Create a stream that skips `offset` bytes and returns at most `length` bytes.
fn create_range_stream<S>(
    stream: S,
    offset: u64,
    length: u64,
) -> impl futures::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>>
where
    S: futures::Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Unpin,
{
    futures::stream::unfold(
        (stream, offset, length, 0u64),
        |(mut stream, offset, length, mut sent)| async move {
            use futures::StreamExt;

            loop {
                match stream.next().await {
                    Some(Ok(data)) => {
                        let data: axum::body::Bytes = data;
                        let data_len = data.len() as u64;

                        // If we haven't reached the offset yet
                        if sent + data_len <= offset {
                            sent += data_len;
                            continue;
                        }

                        // Calculate the slice we need from this chunk
                        let start = if sent < offset {
                            (offset - sent) as usize
                        } else {
                            0
                        };

                        let remaining = length - (sent.saturating_sub(offset).min(length));
                        if remaining == 0 {
                            return None;
                        }

                        let end = (start as u64 + remaining).min(data_len) as usize;

                        sent += data_len;

                        if start < end {
                            let slice = data.slice(start..end);
                            return Some((Ok(slice), (stream, offset, length, sent)));
                        }
                    }
                    Some(Err(e)) => return Some((Err(e), (stream, offset, length, sent))),
                    None => return None,
                }
            }
        },
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::error::{IoErrorContext, Result};
    use futures::StreamExt;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;

    async fn dump_to_vec(path: PathBuf) -> Vec<u8> {
        let stream = NarByteStream::new(path);
        futures::pin_mut!(stream);

        let mut result = Vec::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.expect("Stream error during NAR dump");
            result.extend_from_slice(&bytes);
        }
        result
    }

    #[tokio::test]
    async fn test_dump_store() -> Result<()> {
        let temp_dir =
            harmonia_utils_test::CanonicalTempDir::new().expect("Failed to create temp dir");
        let dir = temp_dir.path().to_path_buf();
        fs::write(dir.join("file"), b"somecontent").io_context("Failed to write test file")?;

        fs::create_dir(dir.join("some_empty_dir")).io_context("Failed to create test empty dir")?;

        let some_dir = dir.join("some_dir");
        fs::create_dir(&some_dir).io_context("Failed to create test dir")?;

        let executable_path = some_dir.join("executable");
        fs::write(&executable_path, b"somescript").io_context("Failed to write test executable")?;
        fs::set_permissions(&executable_path, fs::Permissions::from_mode(0o755))
            .io_context("Failed to set test executable permissions")?;

        std::os::unix::fs::symlink("sometarget", dir.join("symlink"))
            .io_context("Failed to create test symlink")?;

        let nar_dump = dump_to_vec(dir.clone()).await;
        let res = Command::new("nix-store")
            .arg("--dump")
            .arg(dir)
            .output()
            .expect("Failed to run nix-store --dump");
        assert_eq!(res.status.code(), Some(0));
        println!("nar_dump len: {}", nar_dump.len());
        println!("nix-store --dump len: {}", res.stdout.len());
        // println!("nix-store --dump:");
        assert_eq!(res.stdout, nar_dump);

        Ok(())
    }
}

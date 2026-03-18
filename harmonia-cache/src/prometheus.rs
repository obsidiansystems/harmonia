use crate::AppState;
use crate::error;
use axum::extract::MatchedPath;
use axum::extract::State;
use axum::http::Request;
use axum::response::{IntoResponse, Response};
use harmonia_store_remote::PoolMetrics;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, Opts, Registry, TextEncoder,
};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tower::{Layer, Service};

pub struct PrometheusMetrics {
    pub registry: Registry,
    http_requests_total: IntCounterVec,
    http_requests_duration: HistogramVec,
}

impl PrometheusMetrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        let http_requests_total = IntCounterVec::new(
            Opts::new(
                "harmonia_http_requests_total",
                "Total number of HTTP requests",
            ),
            &["method", "path", "status"],
        )?;

        let http_requests_duration = HistogramVec::new(
            HistogramOpts::new(
                "harmonia_http_request_duration_seconds",
                "HTTP request latencies in seconds",
            )
            .buckets(vec![
                0.0001, 0.0005, 0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0,
            ]),
            &["method", "path", "status"],
        )?;

        registry.register(Box::new(http_requests_total.clone()))?;
        registry.register(Box::new(http_requests_duration.clone()))?;

        Ok(PrometheusMetrics {
            registry,
            http_requests_total,
            http_requests_duration,
        })
    }

    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let mut buffer = vec![];
        encoder
            .encode(&self.registry.gather(), &mut buffer)
            .expect("failed to encode Prometheus metrics to buffer");
        String::from_utf8(buffer).expect("prometheus metrics buffer contains invalid UTF-8")
    }
}

#[derive(Clone)]
pub struct PrometheusLayer {
    metrics: Arc<PrometheusMetrics>,
}

impl PrometheusLayer {
    pub fn new(metrics: Arc<PrometheusMetrics>) -> Self {
        PrometheusLayer { metrics }
    }
}

impl<S> Layer<S> for PrometheusLayer {
    type Service = PrometheusMiddleware<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PrometheusMiddleware {
            inner,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PrometheusMiddleware<S> {
    inner: S,
    metrics: Arc<PrometheusMetrics>,
}

impl<S, B> Service<Request<B>> for PrometheusMiddleware<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let start = Instant::now();
        let method = req.method().to_string();
        let path = req
            .extensions()
            .get::<MatchedPath>()
            .map(|p| p.as_str().to_string());
        let metrics = self.metrics.clone();

        let fut = self.inner.call(req);

        Box::pin(async move {
            let res = fut.await?;

            if let Some(path) = path {
                let duration = start.elapsed().as_secs_f64();
                let status = res.status().as_str().to_owned();

                metrics
                    .http_requests_total
                    .with_label_values(&[&method, &path, &status])
                    .inc();

                metrics
                    .http_requests_duration
                    .with_label_values(&[&method, &path, &status])
                    .observe(duration);
            }

            Ok(res)
        })
    }
}

pub async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics.render();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    )
}

pub fn initialize_metrics()
-> Result<(Arc<PrometheusMetrics>, Arc<PoolMetrics>), crate::error::CacheError> {
    let metrics = Arc::new(
        PrometheusMetrics::new().map_err(|e| error::ServerError::Startup {
            reason: format!("Failed to create prometheus metrics: {e}"),
        })?,
    );

    let pool_metrics = Arc::new(
        PoolMetrics::new("harmonia", &metrics.registry).map_err(|e| {
            error::ServerError::Startup {
                reason: format!("Failed to create pool metrics: {e}"),
            }
        })?,
    );

    Ok((metrics, pool_metrics))
}

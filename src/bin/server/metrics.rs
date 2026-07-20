use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

#[derive(Default)]
pub(crate) struct Metrics {
    requests: AtomicU64,
    errors: AtomicU64,
    active: AtomicUsize,
    duration_micros: AtomicU64,
    request_sequence: AtomicU64,
    writer_operations: AtomicU64,
    writer_active: AtomicUsize,
    writer_wait_micros: AtomicU64,
    writer_duration_micros: AtomicU64,
}

impl Metrics {
    fn next_request_id(&self) -> String {
        format!(
            "aq-{}-{}",
            std::process::id(),
            self.request_sequence.fetch_add(1, Ordering::Relaxed) + 1
        )
    }

    pub(crate) fn writer_started(&self) -> Instant {
        self.writer_operations.fetch_add(1, Ordering::Relaxed);
        self.writer_active.fetch_add(1, Ordering::Relaxed);
        Instant::now()
    }

    pub(crate) fn writer_locked(&self, started: Instant) -> Instant {
        self.writer_wait_micros
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        Instant::now()
    }

    pub(crate) fn writer_finished(&self, started: Instant) {
        self.writer_active.fetch_sub(1, Ordering::Relaxed);
        self.writer_duration_micros
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    pub(crate) fn render(
        &self,
        jobs: u64,
        outbox: u64,
        tables: usize,
        segments: usize,
        documents: u64,
    ) -> String {
        format!(
            concat!(
                "# TYPE frankensteindb_http_requests_total counter\n",
                "frankensteindb_http_requests_total {}\n",
                "# TYPE frankensteindb_http_errors_total counter\n",
                "frankensteindb_http_errors_total {}\n",
                "# TYPE frankensteindb_http_active_requests gauge\n",
                "frankensteindb_http_active_requests {}\n",
                "# TYPE frankensteindb_http_request_duration_seconds_total counter\n",
                "frankensteindb_http_request_duration_seconds_total {:.6}\n",
                "frankensteindb_writer_operations_total {}\n",
                "frankensteindb_writer_active {}\n",
                "frankensteindb_writer_wait_seconds_total {:.6}\n",
                "frankensteindb_writer_duration_seconds_total {:.6}\n",
                "frankensteindb_jobs_active {}\n",
                "frankensteindb_outbox_records {}\n",
                "frankensteindb_tables {}\n",
                "frankensteindb_tantivy_segments {}\n",
                "frankensteindb_tantivy_documents {}\n"
            ),
            self.requests.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
            self.active.load(Ordering::Relaxed),
            self.duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            self.writer_operations.load(Ordering::Relaxed),
            self.writer_active.load(Ordering::Relaxed),
            self.writer_wait_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            self.writer_duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            jobs,
            outbox,
            tables,
            segments,
            documents,
        )
    }
}

pub(crate) async fn observe(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| state.metrics.next_request_id());
    request.extensions_mut().insert(request_id.clone());
    state.metrics.requests.fetch_add(1, Ordering::Relaxed);
    state.metrics.active.fetch_add(1, Ordering::Relaxed);
    let mut response = next.run(request).await;
    state.metrics.active.fetch_sub(1, Ordering::Relaxed);
    state.metrics.duration_micros.fetch_add(
        started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
        Ordering::Relaxed,
    );
    if response.status().is_client_error() || response.status().is_server_error() {
        state.metrics.errors.fetch_add(1, Ordering::Relaxed);
    }
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    eprintln!(
        "{}",
        serde_json::json!({
            "event": "http_request",
            "request_id": request_id,
            "method": method,
            "path": path,
            "status": response.status().as_u16(),
            "duration_ms": started.elapsed().as_secs_f64() * 1_000.0,
        })
    );
    response
}

pub(crate) async fn endpoint(State(state): State<AppState>) -> impl IntoResponse {
    let (jobs, outbox) = state.jobs.operational_counts().unwrap_or_default();
    let (tables, segments, documents) = state.search.stats().unwrap_or_default();
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state
            .metrics
            .render(jobs, outbox, tables, segments, documents),
    )
}

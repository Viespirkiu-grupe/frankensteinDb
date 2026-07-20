use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use frankensteindb::{Database, SearchService};
use serde::Serialize;
use serde_json::Value;

use crate::auth::AuthState;
use crate::jobs::{Job, JobStore};
use crate::metrics::Metrics;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) database: Arc<Mutex<Database>>,
    pub(crate) search: SearchService,
    pub(crate) jobs: Arc<JobStore>,
    pub(crate) metrics: Arc<Metrics>,
    locked_tables: Arc<RwLock<HashSet<String>>>,
    pub(crate) auth: Arc<AuthState>,
}

impl AppState {
    pub(crate) fn open(
        path: impl AsRef<Path>,
        api_key: Option<String>,
        api_key_config: Option<std::path::PathBuf>,
    ) -> anyhow::Result<Self> {
        let root = path.as_ref().to_path_buf();
        let database = Database::open(&root)?;
        let search = database.search_service()?;
        Ok(Self {
            database: Arc::new(Mutex::new(database)),
            search,
            jobs: Arc::new(JobStore::open(&root)?),
            metrics: Arc::new(Metrics::default()),
            locked_tables: Arc::new(RwLock::new(HashSet::new())),
            auth: Arc::new(AuthState::open(api_key_config, api_key)?),
        })
    }

    pub(crate) fn lock_table(&self, table: &str) -> WebResult<()> {
        let mut tables = self
            .locked_tables
            .write()
            .map_err(|_| WebError::internal("table lock was poisoned"))?;
        if !tables.insert(table.to_ascii_lowercase()) {
            return Err(WebError::locked(format!(
                "table is already being migrated: {table}"
            )));
        }
        Ok(())
    }

    pub(crate) fn unlock_table(&self, table: &str) {
        if let Ok(mut tables) = self.locked_tables.write() {
            tables.remove(&table.to_ascii_lowercase());
        }
    }

    pub(crate) fn ensure_table_writable(&self, table: &str) -> WebResult<()> {
        let locked = self
            .locked_tables
            .read()
            .map_err(|_| WebError::internal("table lock was poisoned"))?;
        if locked.contains(&table.to_ascii_lowercase()) {
            Err(WebError::locked(format!(
                "table is being migrated: {table}"
            )))
        } else {
            Ok(())
        }
    }

    pub(crate) fn start_job<F>(
        &self,
        kind: &str,
        table: Option<String>,
        task: F,
    ) -> anyhow::Result<Job>
    where
        F: FnOnce(&mut Database, i64, Arc<JobStore>) -> anyhow::Result<Value> + Send + 'static,
    {
        let job = self.jobs.create(kind, table)?;
        let state = self.clone();
        let job_id = job.id;
        tokio::spawn(async move {
            let jobs = state.jobs.clone();
            let task_jobs = jobs.clone();
            let database = state.database.clone();
            let search = state.search.clone();
            let metrics = state.metrics.clone();
            let _ = jobs.running(job_id);
            let outcome = tokio::task::spawn_blocking(move || {
                let waiting = metrics.writer_started();
                let mut database = match database.lock() {
                    Ok(database) => database,
                    Err(_) => {
                        metrics.writer_finished(waiting);
                        return Err(anyhow::anyhow!("database lock was poisoned"));
                    }
                };
                let executing = metrics.writer_locked(waiting);
                let outcome = (|| {
                    let result = task(&mut database, job_id, task_jobs)?;
                    search.publish_catalog(database.tables()?)?;
                    Ok::<_, anyhow::Error>(result)
                })();
                metrics.writer_finished(executing);
                outcome
            })
            .await;
            match outcome {
                Ok(Ok(result)) => {
                    let _ = jobs.complete(job_id, result);
                }
                Ok(Err(error)) => {
                    let _ = jobs.fail(job_id, format!("{error:#}"));
                }
                Err(error) => {
                    let _ = jobs.fail(job_id, error.to_string());
                }
            }
        });
        Ok(job)
    }

    pub(crate) async fn with_writer<T, F>(&self, operation: F) -> WebResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Database) -> anyhow::Result<T> + Send + 'static,
    {
        let database = self.database.clone();
        let search = self.search.clone();
        let metrics = self.metrics.clone();
        tokio::task::spawn_blocking(move || {
            let waiting = metrics.writer_started();
            let mut database = match database.lock() {
                Ok(database) => database,
                Err(_) => {
                    metrics.writer_finished(waiting);
                    return Err(anyhow::anyhow!("database lock was poisoned"));
                }
            };
            let executing = metrics.writer_locked(waiting);
            let outcome = (|| {
                let result = operation(&mut database)?;
                search.publish_catalog(database.tables()?)?;
                Ok::<T, anyhow::Error>(result)
            })();
            metrics.writer_finished(executing);
            outcome
        })
        .await
        .map_err(|error| WebError::internal(error.to_string()))?
        .map_err(WebError::from)
    }

    pub(crate) async fn with_search<T, F>(&self, operation: F) -> WebResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&SearchService) -> anyhow::Result<T> + Send + 'static,
    {
        let search = self.search.clone();
        tokio::task::spawn_blocking(move || operation(&search))
            .await
            .map_err(|error| WebError::internal(error.to_string()))?
            .map_err(WebError::from)
    }
}

pub(crate) type WebResult<T> = Result<T, WebError>;

#[derive(Debug)]
pub(crate) struct WebError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl std::fmt::Display for WebError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WebError {}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl WebError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }

    pub(crate) fn precondition_failed(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
            message,
        )
    }

    pub(crate) fn locked(message: impl Into<String>) -> Self {
        Self::new(StatusCode::LOCKED, "locked", message)
    }

    pub(crate) fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method not allowed for this route",
        )
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for WebError {
    fn from(error: anyhow::Error) -> Self {
        let message = format!("{error:#}");
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("table not found") || normalized.contains("row not found") {
            Self::not_found(message)
        } else if normalized.contains("etag mismatch") {
            Self::precondition_failed("row ETag no longer matches")
        } else if normalized.contains("already exists")
            || normalized.contains("unique constraint failed")
        {
            Self::conflict(message)
        } else {
            Self::bad_request(message)
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

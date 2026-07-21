use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio_util::io::ReaderStream;

use crate::api_types::DataResponse;
use crate::jobs::AuditRecord;
use crate::jobs::Job;
use crate::state::{AppState, WebError, WebResult};
use frankensteindb::OptimizeOptions;

pub(crate) async fn list(State(state): State<AppState>) -> WebResult<Json<DataResponse<Vec<Job>>>> {
    Ok(Json(DataResponse::new(
        state.jobs.list().map_err(WebError::from)?,
    )))
}

pub(crate) async fn audit(
    State(state): State<AppState>,
) -> WebResult<Json<DataResponse<Vec<AuditRecord>>>> {
    Ok(Json(DataResponse::new(
        state.jobs.audit_records(1_000).map_err(WebError::from)?,
    )))
}

pub(crate) async fn artifact(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    let job = state
        .jobs
        .get(id)
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found(format!("job not found: {id}")))?;
    if job.state != "completed" {
        return Err(WebError::conflict("job artifact is not ready"));
    }
    let path = state.jobs.artifact_path(id, "tar.zst");
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| WebError::not_found("job has no artifact"))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zstd"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=frankensteindb-backup.tar.zst",
            ),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> WebResult<Json<DataResponse<Job>>> {
    let job = state
        .jobs
        .get(id)
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found(format!("job not found: {id}")))?;
    Ok(Json(DataResponse::new(job)))
}

pub(crate) async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> WebResult<StatusCode> {
    if state.jobs.cancel(id).map_err(WebError::from)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(WebError::conflict("job is not cancellable"))
    }
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> WebResult<(StatusCode, Json<DataResponse<Job>>)> {
    let original = state
        .jobs
        .get(id)
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::not_found(format!("job not found: {id}")))?;
    if !matches!(
        original.state.as_str(),
        "failed" | "interrupted" | "cancelled"
    ) {
        return Err(WebError::conflict(
            "only failed, interrupted, or cancelled jobs can be retried",
        ));
    }
    let table = original.table.clone();
    let job = match original.kind.as_str() {
        "reindex" => {
            let name = table
                .clone()
                .ok_or_else(|| WebError::bad_request("reindex job has no table"))?;
            state.start_job("reindex", table, move |database, _, _| {
                Ok(serde_json::json!({"message": database.reindex_table(&name)?.message}))
            })
        }
        "optimize" => {
            let name = table
                .clone()
                .ok_or_else(|| WebError::bad_request("optimize job has no table"))?;
            let options = original
                .input
                .clone()
                .map(serde_json::from_value::<OptimizeOptions>)
                .transpose()
                .map_err(|error| WebError::bad_request(error.to_string()))?
                .unwrap_or_default();
            let input = serde_json::json!(options);
            state.start_job_with_input("optimize", table, Some(input), move |database, _, _| {
                Ok(serde_json::to_value(
                    database.optimize_table_with_options(&name, options)?,
                )?)
            })
        }
        "backup" => state.start_job("backup", None, move |database, id, jobs| {
            let path = jobs.artifact_path(id, "tar.zst");
            database.backup_to(&path)?;
            Ok(serde_json::json!({"artifact": format!("/api/v1/jobs/{id}/artifact")}))
        }),
        _ => return Err(WebError::conflict("this job no longer has retryable input")),
    }
    .map_err(WebError::from)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

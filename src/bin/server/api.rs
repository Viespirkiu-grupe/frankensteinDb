use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use frankensteindb::{OptimizeOptions, QueryResult, SchemaChange, TableDef};

use crate::aggregation_api;
use crate::api_rows;
use crate::api_types::{ApiJson, DataResponse, FacetBody, OperationResponse};
use crate::diagnostics_api;
use crate::import_api;
use crate::job_api;
use crate::similar_api;
use crate::state::{AppState, WebError, WebResult};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tables", get(list_tables).post(create_table))
        .route("/api/v1/tables/{table}", get(get_table).delete(drop_table))
        .route(
            "/api/v1/tables/{table}/rows",
            get(api_rows::list_rows)
                .post(api_rows::insert_row)
                .patch(api_rows::update_rows)
                .delete(api_rows::delete_rows),
        )
        .route(
            "/api/v1/tables/{table}/rows/{key}",
            get(api_rows::get_row)
                .put(api_rows::replace_row)
                .patch(api_rows::patch_row)
                .delete(api_rows::delete_row),
        )
        .route("/api/v1/tables/{table}/query", post(api_rows::read))
        .route(
            "/api/v1/tables/{table}/aggregate-intermediate",
            post(aggregation_api::intermediate),
        )
        .route(
            "/api/v1/tables/{table}/aggregate-merge",
            post(aggregation_api::merge),
        )
        .route(
            "/api/v1/tables/{table}/explain",
            post(diagnostics_api::explain),
        )
        .route(
            "/api/v1/tables/{table}/rows/{key}/explain-score",
            post(diagnostics_api::explain_score),
        )
        .route(
            "/api/v1/tables/{table}/profile",
            post(diagnostics_api::profile),
        )
        .route(
            "/api/v1/tables/{table}/rows/{key}/similar",
            post(similar_api::find),
        )
        .route("/api/v1/tables/{table}/facets/{column}", post(facets))
        .route(
            "/api/v1/tables/{table}/aliases",
            axum::routing::put(set_aliases),
        )
        .route("/api/v1/tables/{table}/reindex", post(reindex))
        .route("/api/v1/tables/{table}/optimize", post(optimize))
        .route("/api/v1/mutations", post(api_rows::mutate_batch))
        .route("/api/v1/flush", post(flush))
        .route("/api/v1/jobs", get(job_api::list))
        .route(
            "/api/v1/jobs/{id}",
            get(job_api::get).delete(job_api::cancel),
        )
        .route("/api/v1/jobs/{id}/retry", post(job_api::retry))
        .route("/api/v1/auth/reload", post(reload_auth))
        .route("/api/v1/audit", get(job_api::audit))
        .route("/api/v1/backups", post(create_backup))
        .route("/api/v1/tables/{table}/schema-changes", post(change_schema))
        .route("/api/v1/jobs/{id}/artifact", get(job_api::artifact))
        .route("/api/v1/tables/{table}/imports", post(import_api::create))
}

async fn facets(
    State(state): State<AppState>,
    Path((table, column)): Path<(String, String)>,
    ApiJson(body): ApiJson<FacetBody>,
) -> WebResult<Json<DataResponse<serde_json::Value>>> {
    let value = state
        .with_search(move |search| {
            if body.exclude_own_filter {
                search.facets_excluding_own_filter(
                    &table,
                    &column,
                    &body.root,
                    body.limit,
                    body.filter.as_ref(),
                )
            } else {
                search.facets(
                    &table,
                    &column,
                    &body.root,
                    body.limit,
                    body.filter.as_ref(),
                )
            }
        })
        .await?;
    Ok(Json(DataResponse::new(value)))
}

async fn set_aliases(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(aliases): ApiJson<Vec<String>>,
) -> WebResult<Json<DataResponse<TableDef>>> {
    let definition = state
        .with_writer(move |database| database.set_table_aliases(&table, aliases))
        .await?;
    Ok(Json(DataResponse::new(definition)))
}

pub(crate) async fn openapi() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        include_str!("../../../docs/openapi.json"),
    )
        .into_response()
}

async fn list_tables(
    State(state): State<AppState>,
) -> WebResult<Json<DataResponse<Vec<TableDef>>>> {
    let tables = state.with_search(|search| search.tables()).await?;
    Ok(Json(DataResponse::new(tables)))
}

async fn get_table(
    State(state): State<AppState>,
    Path(table): Path<String>,
) -> WebResult<Json<DataResponse<TableDef>>> {
    let requested = table.clone();
    let table = state
        .with_search(move |search| search.table(&table))
        .await
        .map_err(|_| WebError::not_found(format!("table not found: {requested}")))?;
    Ok(Json(DataResponse::new(table)))
}

async fn create_table(
    State(state): State<AppState>,
    ApiJson(def): ApiJson<TableDef>,
) -> WebResult<(StatusCode, Json<DataResponse<TableDef>>)> {
    let response = def.clone();
    state
        .with_writer(move |database| database.create_table_def(def))
        .await?;
    Ok((StatusCode::CREATED, Json(DataResponse::new(response))))
}

async fn drop_table(
    State(state): State<AppState>,
    Path(table): Path<String>,
) -> WebResult<StatusCode> {
    state
        .with_writer(move |database| database.drop_table_named(&table))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reindex(
    State(state): State<AppState>,
    Path(table): Path<String>,
) -> WebResult<(StatusCode, Json<DataResponse<crate::jobs::Job>>)> {
    let job = state
        .start_job("reindex", Some(table.clone()), move |database, _, _| {
            let result = database.reindex_table(&table)?;
            Ok(serde_json::json!({"message": result.message}))
        })
        .map_err(WebError::from)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

async fn optimize(
    State(state): State<AppState>,
    Path(table): Path<String>,
    body: Option<Json<OptimizeOptions>>,
) -> WebResult<(StatusCode, Json<DataResponse<crate::jobs::Job>>)> {
    let options = body.map(|Json(options)| options).unwrap_or_default();
    let input = serde_json::json!(options);
    let job = state
        .start_job_with_input(
            "optimize",
            Some(table.clone()),
            Some(input),
            move |database, _, _| {
                Ok(serde_json::to_value(
                    database.optimize_table_with_options(&table, options)?,
                )?)
            },
        )
        .map_err(WebError::from)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

async fn flush(State(state): State<AppState>) -> WebResult<StatusCode> {
    state.with_writer(|database| database.flush()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reload_auth(
    State(state): State<AppState>,
) -> WebResult<Json<DataResponse<serde_json::Value>>> {
    let count = state.auth.reload().map_err(WebError::from)?;
    Ok(Json(DataResponse::new(
        serde_json::json!({"keys_loaded": count}),
    )))
}

async fn create_backup(
    State(state): State<AppState>,
) -> WebResult<(StatusCode, Json<DataResponse<crate::jobs::Job>>)> {
    let job = state
        .start_job("backup", None, move |database, id, jobs| {
            let path = jobs.artifact_path(id, "tar.zst");
            database.backup_to(&path)?;
            Ok(serde_json::json!({
                "artifact": format!("/api/v1/jobs/{id}/artifact"),
                "bytes": std::fs::metadata(path)?.len()
            }))
        })
        .map_err(WebError::from)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

async fn change_schema(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(changes): ApiJson<Vec<SchemaChange>>,
) -> WebResult<(StatusCode, Json<DataResponse<crate::jobs::Job>>)> {
    state.lock_table(&table)?;
    let unlock = state.clone();
    let migrated_table = table.clone();
    let task_table = table.clone();
    let job = state
        .start_job(
            "schema_change",
            Some(table.clone()),
            move |database, _, _| {
                let result = database
                    .change_table_schema(&task_table, changes)
                    .map(|result| serde_json::json!({"message": result.message}));
                unlock.unlock_table(&migrated_table);
                result
            },
        )
        .map_err(|error| {
            state.unlock_table(&table);
            WebError::from(error)
        })?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

pub(crate) fn operation_response(result: QueryResult, deferred: bool) -> OperationResponse {
    OperationResponse::from_result(
        result,
        Some(if deferred { "deferred" } else { "published" }),
    )
}

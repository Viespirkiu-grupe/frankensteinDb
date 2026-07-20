use axum::Json;
use axum::extract::{Path, State};
use serde_json::Value;

use crate::api_rows::key_filter;
use crate::api_types::{ApiJson, DataResponse, ReadBody, RowsResponse};
use crate::state::{AppState, WebResult};

pub(crate) async fn explain(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(body): ApiJson<ReadBody>,
) -> WebResult<Json<RowsResponse>> {
    let request = body.into_request(table);
    let result = state
        .with_search(move |search| search.explain(&request))
        .await?;
    Ok(Json(RowsResponse::from_result(result, 1, 0)))
}

pub(crate) async fn explain_score(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
    ApiJson(body): ApiJson<ReadBody>,
) -> WebResult<Json<DataResponse<Value>>> {
    let result = state
        .with_search(move |search| {
            let def = search.table(&table)?;
            let identity = key_filter(&def, &key)?;
            let request = body.into_request(def.name);
            search.explain_score(&request, &identity)
        })
        .await?;
    Ok(Json(DataResponse::new(result)))
}

pub(crate) async fn profile(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(body): ApiJson<ReadBody>,
) -> WebResult<Json<DataResponse<Value>>> {
    let request = body.into_request(table);
    let result = state
        .with_search(move |search| search.profile(request))
        .await?;
    Ok(Json(DataResponse::new(result)))
}

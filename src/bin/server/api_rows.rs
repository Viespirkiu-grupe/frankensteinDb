use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use frankensteindb::{ColumnType, Comparison, Filter, Mutation, ReadRequest, Sort, TableDef};
use serde::Deserialize;
use serde_json::Value;

use crate::api::operation_response;
use crate::api_types::{
    ApiJson, ApiQuery, DataResponse, DeleteBody, MutationOptions, OperationResponse, ReadBody,
    RowsResponse, UpdateBody, WriteOptions,
};
use crate::row_concurrency::{apply_key_mutation, row_etag};
use crate::state::{AppState, WebError, WebResult};

#[derive(Debug, Deserialize)]
pub(crate) struct ListRowsParams {
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

pub(crate) async fn list_rows(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiQuery(params): ApiQuery<ListRowsParams>,
) -> WebResult<Json<RowsResponse>> {
    let limit = params.limit.clamp(1, 1_000);
    let offset = params.offset;
    let result = state
        .with_search(move |search| {
            let def = search.table(&table)?;
            search.read(list_request(&def, params, limit)?)
        })
        .await?;
    Ok(Json(RowsResponse::from_result(result, limit, offset)))
}

pub(crate) async fn get_row(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
) -> WebResult<Response> {
    let requested = key.clone();
    let result = state
        .with_search(move |search| {
            let def = search.table(&table)?;
            search.read(row_request(&def, &key)?)
        })
        .await?;
    let row = RowsResponse::from_result(result, 1, 0)
        .data
        .into_iter()
        .next()
        .ok_or_else(|| WebError::not_found(format!("row not found: {requested}")))?;
    let etag = row_etag(&row)?;
    let mut response = Json(DataResponse::new(row)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|error| WebError::internal(error.to_string()))?,
    );
    Ok(response)
}

pub(crate) async fn read(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(body): ApiJson<ReadBody>,
) -> WebResult<Response> {
    let aggregations = body.aggregations.clone();
    let request = body.into_request(table);
    if !aggregations.is_empty() && request.search_after.is_some() {
        return Err(WebError::bad_request(
            "search_after cannot be combined with aggregations",
        ));
    }
    let limit = request.limit;
    let offset = request.offset;
    let result = state
        .with_search(move |search| {
            let aggregation_result = if aggregations.is_empty() {
                None
            } else {
                Some(search.aggregate(&request.table, request.filter.as_ref(), aggregations)?)
            };
            let rows = if request.limit == 0 {
                frankensteindb::QueryResult {
                    columns: vec![],
                    rows: vec![],
                    message: "0 row(s)".into(),
                    next_search_after: None,
                }
            } else {
                search.read(request)?
            };
            Ok((rows, aggregation_result))
        })
        .await?;
    let rows = RowsResponse::from_result(result.0, limit, offset);
    if let Some(aggregations) = result.1 {
        Ok(Json(serde_json::json!({
            "data": rows.data,
            "meta": rows.meta,
            "aggregations": aggregations,
        }))
        .into_response())
    } else {
        Ok(Json(rows).into_response())
    }
}

pub(crate) async fn insert_row(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiQuery(options): ApiQuery<WriteOptions>,
    ApiJson(row): ApiJson<BTreeMap<String, Value>>,
) -> WebResult<(StatusCode, Json<OperationResponse>)> {
    state.ensure_table_writable(&table)?;
    let deferred = options.deferred;
    let response = apply_mutation(&state, Mutation::Insert { table, row }, deferred).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn patch_row(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
    ApiQuery(options): ApiQuery<WriteOptions>,
    ApiJson(values): ApiJson<BTreeMap<String, Value>>,
) -> WebResult<Json<OperationResponse>> {
    state.ensure_table_writable(&table)?;
    let filter = table_key_filter(&state, &table, &key).await?;
    let response = apply_key_mutation(
        &state,
        &table,
        &key,
        &headers,
        Mutation::Update {
            table: table.clone(),
            values,
            filter,
        },
        options.deferred,
    )
    .await?;
    ensure_single_row_changed(&response, &key)?;
    Ok(Json(response))
}

pub(crate) async fn replace_row(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
    ApiQuery(options): ApiQuery<WriteOptions>,
    ApiJson(mut values): ApiJson<BTreeMap<String, Value>>,
) -> WebResult<Json<OperationResponse>> {
    state.ensure_table_writable(&table)?;
    let requested_table = table.clone();
    let def = state
        .with_search(move |search| search.table(&requested_table))
        .await?;
    values.remove(&primary_key(&def).name);
    for column in def.columns.iter().filter(|column| !column.primary_key) {
        if !values.contains_key(&column.name) {
            if column.nullable {
                values.insert(column.name.clone(), Value::Null);
            } else {
                return Err(WebError::bad_request(format!(
                    "PUT requires non-nullable column: {}",
                    column.name
                )));
            }
        }
    }
    let filter = key_filter(&def, &key).map_err(WebError::from)?;
    let response = apply_key_mutation(
        &state,
        &table,
        &key,
        &headers,
        Mutation::Update {
            table: table.clone(),
            values,
            filter,
        },
        options.deferred,
    )
    .await?;
    ensure_single_row_changed(&response, &key)?;
    Ok(Json(response))
}

pub(crate) async fn delete_row(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
    headers: HeaderMap,
    ApiQuery(options): ApiQuery<WriteOptions>,
) -> WebResult<Json<OperationResponse>> {
    state.ensure_table_writable(&table)?;
    let filter = table_key_filter(&state, &table, &key).await?;
    let response = apply_key_mutation(
        &state,
        &table,
        &key,
        &headers,
        Mutation::Delete {
            table: table.clone(),
            filter,
        },
        options.deferred,
    )
    .await?;
    ensure_single_row_changed(&response, &key)?;
    Ok(Json(response))
}

pub(crate) async fn update_rows(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiQuery(options): ApiQuery<MutationOptions>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> WebResult<Json<OperationResponse>> {
    state.ensure_table_writable(&table)?;
    if options.dry_run {
        return Ok(Json(preview_mutation(&state, &table, body.filter).await?));
    }
    Ok(Json(
        apply_limited_mutation(
            &state,
            Mutation::Update {
                table,
                values: body.values,
                filter: body.filter,
            },
            options.deferred,
            options.max_rows,
        )
        .await?,
    ))
}

pub(crate) async fn delete_rows(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiQuery(options): ApiQuery<MutationOptions>,
    ApiJson(body): ApiJson<DeleteBody>,
) -> WebResult<Json<OperationResponse>> {
    state.ensure_table_writable(&table)?;
    if options.dry_run {
        return Ok(Json(preview_mutation(&state, &table, body.filter).await?));
    }
    Ok(Json(
        apply_limited_mutation(
            &state,
            Mutation::Delete {
                table,
                filter: body.filter,
            },
            options.deferred,
            options.max_rows,
        )
        .await?,
    ))
}

pub(crate) async fn mutate_batch(
    State(state): State<AppState>,
    ApiJson(mutations): ApiJson<Vec<Mutation>>,
) -> WebResult<Json<DataResponse<Vec<OperationResponse>>>> {
    for mutation in &mutations {
        let table = match mutation {
            Mutation::Insert { table, .. }
            | Mutation::Update { table, .. }
            | Mutation::Delete { table, .. } => table,
        };
        state.ensure_table_writable(table)?;
    }
    let results = state
        .with_writer(move |database| database.mutate_batch_typed(mutations))
        .await?;
    let responses = results
        .into_iter()
        .map(|result| operation_response(result, false))
        .collect();
    Ok(Json(DataResponse::new(responses)))
}

async fn apply_mutation(
    state: &AppState,
    mutation: Mutation,
    deferred: bool,
) -> WebResult<OperationResponse> {
    let result = state
        .with_writer(move |database| {
            if deferred {
                database.mutate_typed_deferred(mutation)
            } else {
                database.mutate_typed(mutation)
            }
        })
        .await?;
    Ok(operation_response(result, deferred))
}

async fn apply_limited_mutation(
    state: &AppState,
    mutation: Mutation,
    deferred: bool,
    max_rows: Option<usize>,
) -> WebResult<OperationResponse> {
    if deferred && max_rows.is_some() {
        return Err(WebError::bad_request(
            "max_rows cannot be combined with deferred writes",
        ));
    }
    let result = state
        .with_writer(move |database| {
            if deferred {
                database.mutate_typed_deferred(mutation)
            } else {
                database.mutate_typed_limited(mutation, max_rows)
            }
        })
        .await?;
    Ok(operation_response(result, deferred))
}

async fn preview_mutation(
    state: &AppState,
    table: &str,
    filter: Filter,
) -> WebResult<OperationResponse> {
    let table = table.to_owned();
    let result = state
        .with_search(move |search| {
            search.read(ReadRequest {
                table,
                projection: vec![frankensteindb::Projection::Aggregate {
                    function: frankensteindb::Aggregate::Count,
                    column: None,
                    alias: "matches".into(),
                }],
                filter: Some(filter),
                group_by: vec![],
                order_by: vec![],
                limit: 1,
                offset: 0,
                search_after: None,
                min_score: None,
            })
        })
        .await?;
    let count = result
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    Ok(OperationResponse {
        message: format!("{count} row(s) matched; no changes applied"),
        affected_rows: Some(count),
        visibility: Some("dry_run"),
    })
}

fn ensure_single_row_changed(response: &OperationResponse, key: &str) -> WebResult<()> {
    if response.affected_rows == Some(0) {
        Err(WebError::not_found(format!("row not found: {key}")))
    } else {
        Ok(())
    }
}

async fn table_key_filter(state: &AppState, table: &str, key: &str) -> WebResult<Filter> {
    let table = table.to_owned();
    let key = key.to_owned();
    state
        .with_search(move |search| key_filter(&search.table(&table)?, &key))
        .await
}

fn list_request(
    def: &TableDef,
    params: ListRowsParams,
    limit: usize,
) -> anyhow::Result<ReadRequest> {
    let sort = params
        .sort
        .as_deref()
        .map(|name| find_column(def, name))
        .transpose()?
        .unwrap_or_else(|| primary_key(def));
    let descending = match params.order.as_deref() {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(value) => anyhow::bail!("invalid order: {value}; expected asc or desc"),
    };
    Ok(ReadRequest {
        table: def.name.clone(),
        projection: Vec::new(),
        filter: params
            .q
            .map(|query| query.trim().to_owned())
            .filter(|query| !query.is_empty())
            .map(|query| Filter::Search {
                fields: Vec::new(),
                query,
            }),
        group_by: Vec::new(),
        order_by: vec![Sort {
            column: sort.name.clone(),
            json_path: None,
            json_type: None,
            descending,
        }],
        limit,
        offset: params.offset,
        search_after: None,
        min_score: None,
    })
}

fn row_request(def: &TableDef, key: &str) -> anyhow::Result<ReadRequest> {
    Ok(ReadRequest {
        table: def.name.clone(),
        projection: Vec::new(),
        filter: Some(key_filter(def, key)?),
        group_by: Vec::new(),
        order_by: Vec::new(),
        limit: 1,
        offset: 0,
        search_after: None,
        min_score: None,
    })
}

pub(crate) fn key_filter(def: &TableDef, key: &str) -> anyhow::Result<Filter> {
    let primary_key = primary_key(def);
    let value = match primary_key.data_type {
        ColumnType::Integer => Value::from(key.parse::<i64>()?),
        ColumnType::Text => Value::String(key.to_owned()),
        _ => anyhow::bail!("unsupported primary key type"),
    };
    Ok(Filter::Compare {
        column: primary_key.name.clone(),
        operator: Comparison::Equal,
        value,
    })
}

fn primary_key(def: &TableDef) -> &frankensteindb::ColumnDef {
    def.columns
        .iter()
        .find(|column| column.primary_key)
        .expect("validated table has a primary key")
}

fn find_column<'a>(def: &'a TableDef, name: &str) -> anyhow::Result<&'a frankensteindb::ColumnDef> {
    def.columns
        .iter()
        .find(|column| column.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown column: {name}"))
}

const fn default_limit() -> usize {
    100
}

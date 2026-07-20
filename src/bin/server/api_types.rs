use axum::Json;
use axum::extract::{
    FromRequest, FromRequestParts, Query, Request,
    rejection::{JsonRejection, QueryRejection},
};
use axum::http::request::Parts;
use frankensteindb::{Aggregation, Filter, Projection, QueryResult, ReadRequest, Sort};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::state::WebError;

#[derive(Debug, Serialize)]
pub(crate) struct DataResponse<T> {
    pub(crate) data: T,
}

impl<T> DataResponse<T> {
    pub(crate) fn new(data: T) -> Self {
        Self { data }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RowsResponse {
    pub(crate) data: Vec<Map<String, Value>>,
    pub(crate) meta: RowsMeta,
}

#[derive(Debug, Serialize)]
pub(crate) struct RowsMeta {
    pub(crate) columns: Vec<String>,
    pub(crate) count: usize,
    pub(crate) limit: usize,
    pub(crate) offset: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_search_after: Option<Vec<Value>>,
}

impl RowsResponse {
    pub(crate) fn from_result(result: QueryResult, limit: usize, offset: usize) -> Self {
        let next_search_after = result.next_search_after;
        let columns = result.columns;
        let data = result
            .rows
            .into_iter()
            .map(|row| columns.iter().cloned().zip(row).collect())
            .collect::<Vec<_>>();
        Self {
            meta: RowsMeta {
                columns,
                count: data.len(),
                limit,
                offset,
                next_search_after,
            },
            data,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct OperationResponse {
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) affected_rows: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) visibility: Option<&'static str>,
}

impl OperationResponse {
    pub(crate) fn from_result(result: QueryResult, visibility: Option<&'static str>) -> Self {
        Self {
            affected_rows: result
                .message
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok()),
            message: result.message,
            visibility,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WriteOptions {
    #[serde(default)]
    pub(crate) deferred: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MutationOptions {
    #[serde(default)]
    pub(crate) deferred: bool,
    #[serde(default)]
    pub(crate) dry_run: bool,
    pub(crate) max_rows: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadBody {
    #[serde(default)]
    projection: Vec<Projection>,
    #[serde(default)]
    filter: Option<Filter>,
    #[serde(default)]
    group_by: Vec<String>,
    #[serde(default)]
    order_by: Vec<Sort>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    search_after: Option<Vec<Value>>,
    #[serde(default)]
    min_score: Option<f32>,
    #[serde(default)]
    pub(crate) aggregations: std::collections::BTreeMap<String, Aggregation>,
}

impl ReadBody {
    pub(crate) fn into_request(self, table: String) -> ReadRequest {
        ReadRequest {
            table,
            projection: self.projection,
            filter: self.filter,
            group_by: self.group_by,
            order_by: self.order_by,
            limit: self.limit.min(10_000),
            offset: self.offset,
            search_after: self.search_after,
            min_score: self.min_score,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBody {
    pub(crate) filter: Filter,
    pub(crate) values: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteBody {
    pub(crate) filter: Filter,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FacetBody {
    #[serde(default = "default_facet_root")]
    pub(crate) root: String,
    #[serde(default = "default_facet_limit")]
    pub(crate) limit: usize,
    #[serde(default)]
    pub(crate) filter: Option<Filter>,
}

fn default_facet_root() -> String {
    "/".into()
}
const fn default_facet_limit() -> usize {
    100
}

pub(crate) struct ApiJson<T>(pub(crate) T);
pub(crate) struct ApiQuery<T>(pub(crate) T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = WebError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(json_rejection)
    }
}

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: serde::de::DeserializeOwned,
{
    type Rejection = WebError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(query_rejection)
    }
}

fn json_rejection(rejection: JsonRejection) -> WebError {
    WebError::bad_request(rejection.body_text())
}

fn query_rejection(rejection: QueryRejection) -> WebError {
    WebError::bad_request(rejection.body_text())
}

const fn default_limit() -> usize {
    100
}

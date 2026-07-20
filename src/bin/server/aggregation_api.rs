use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use frankensteindb::Aggregation;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_types::{ApiJson, DataResponse, ReadBody};
use crate::state::{AppState, WebError, WebResult};

const MAX_INTERMEDIATE_PAYLOADS: usize = 1_024;
const MAX_INTERMEDIATE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub(crate) struct IntermediateResponse {
    format: &'static str,
    payload_hex: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MergeBody {
    aggregations: BTreeMap<String, Aggregation>,
    payloads_hex: Vec<String>,
}

/// Collects one shard's mergeable Tantivy aggregation fruit.
pub(crate) async fn intermediate(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(body): ApiJson<ReadBody>,
) -> WebResult<Json<DataResponse<IntermediateResponse>>> {
    let aggregations = body.aggregations.clone();
    if aggregations.is_empty() {
        return Err(WebError::bad_request("aggregations cannot be empty"));
    }
    let request = body.into_request(table);
    let payload = state
        .with_search(move |search| {
            search.aggregate_intermediate(&request.table, request.filter.as_ref(), aggregations)
        })
        .await?;
    Ok(Json(DataResponse::new(IntermediateResponse {
        format: "tantivy-bincode-v1",
        payload_hex: hex::encode(payload),
    })))
}

/// Merges trusted shard fruits after validating version and aggregation identity.
pub(crate) async fn merge(
    State(state): State<AppState>,
    Path(table): Path<String>,
    ApiJson(body): ApiJson<MergeBody>,
) -> WebResult<Json<DataResponse<Value>>> {
    if body.aggregations.is_empty() {
        return Err(WebError::bad_request("aggregations cannot be empty"));
    }
    let payloads = decode_payloads(&body.payloads_hex)?;
    let value = state
        .with_search(move |search| {
            search.merge_aggregation_intermediates(&table, body.aggregations, &payloads)
        })
        .await?;
    Ok(Json(DataResponse::new(value)))
}

fn decode_payloads(encoded: &[String]) -> WebResult<Vec<Vec<u8>>> {
    if encoded.is_empty() || encoded.len() > MAX_INTERMEDIATE_PAYLOADS {
        return Err(WebError::bad_request(
            "payloads_hex must contain 1..=1024 payloads",
        ));
    }
    let mut total = 0usize;
    encoded
        .iter()
        .map(|payload| {
            let payload = hex::decode(payload)
                .map_err(|_| WebError::bad_request("payloads_hex contains invalid hex"))?;
            total = total
                .checked_add(payload.len())
                .ok_or_else(|| WebError::bad_request("intermediate payload size overflow"))?;
            if total > MAX_INTERMEDIATE_BYTES {
                return Err(WebError::bad_request(
                    "intermediate payloads exceed 256 MiB",
                ));
            }
            Ok(payload)
        })
        .collect()
}

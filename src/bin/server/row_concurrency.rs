use axum::http::{HeaderMap, header};
use frankensteindb::Mutation;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::api::operation_response;
use crate::api_types::OperationResponse;
use crate::state::{AppState, WebError, WebResult};

pub(crate) async fn apply_key_mutation(
    state: &AppState,
    table: &str,
    key: &str,
    headers: &HeaderMap,
    mutation: Mutation,
    deferred: bool,
) -> WebResult<OperationResponse> {
    let expected = if_match(headers)?;
    let table = table.to_owned();
    let key = key.to_owned();
    let result = state
        .with_writer(move |database| {
            if let Some(expected) = expected {
                database.ensure_row_etag(&table, &key, &expected)?;
            }
            if deferred {
                database.mutate_typed_deferred(mutation)
            } else {
                database.mutate_typed(mutation)
            }
        })
        .await?;
    Ok(operation_response(result, deferred))
}

pub(crate) fn row_etag(row: &Map<String, Value>) -> WebResult<String> {
    let bytes = serde_json::to_vec(row).map_err(|error| WebError::internal(error.to_string()))?;
    Ok(format!("\"{}\"", hex::encode(Sha256::digest(bytes))))
}

fn if_match(headers: &HeaderMap) -> WebResult<Option<String>> {
    let Some(expected) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    Ok(Some(
        expected
            .to_str()
            .map_err(|_| WebError::bad_request("invalid If-Match header"))?
            .to_owned(),
    ))
}

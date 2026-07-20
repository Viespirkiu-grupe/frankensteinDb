use axum::Json;
use axum::extract::{Path, State};
use frankensteindb::{ColumnType, MoreLikeThisOptions};
use serde_json::Value;

use crate::api_types::{ApiJson, RowsResponse};
use crate::state::{AppState, WebError, WebResult};

pub(crate) async fn find(
    State(state): State<AppState>,
    Path((table, key)): Path<(String, String)>,
    ApiJson(options): ApiJson<MoreLikeThisOptions>,
) -> WebResult<Json<RowsResponse>> {
    let requested_table = table.clone();
    let definition = state
        .with_search(move |search| search.table(&requested_table))
        .await?;
    let primary_key = definition
        .columns
        .iter()
        .find(|column| column.primary_key)
        .expect("validated table has a primary key");
    let key = match primary_key.data_type {
        ColumnType::Integer => Value::from(
            key.parse::<i64>()
                .map_err(|error| WebError::bad_request(error.to_string()))?,
        ),
        ColumnType::Text => Value::String(key),
        _ => return Err(WebError::bad_request("unsupported primary key type")),
    };
    let limit = options.limit;
    let result = state
        .with_search(move |search| search.more_like_this(&table, key, options))
        .await?;
    Ok(Json(RowsResponse::from_result(result, limit, 0)))
}

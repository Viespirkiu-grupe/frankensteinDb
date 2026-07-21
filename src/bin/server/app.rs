use axum::Json;
use axum::Router;
use axum::middleware;
use axum::routing::get;

use crate::api;
use crate::auth;
use crate::metrics;
use crate::state::AppState;
use crate::state::WebError;

pub(crate) fn router(state: AppState) -> Router {
    let protected = api::router().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_bearer,
    ));
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(api::openapi))
        .route("/metrics", get(metrics::endpoint))
        .merge(protected)
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            metrics::observe,
        ))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn not_found() -> WebError {
    WebError::not_found("route not found")
}

async fn method_not_allowed() -> WebError {
    WebError::method_not_allowed()
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn bearer_failure_is_a_json_error() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), Some("secret".into()), None).unwrap());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tables")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
            "unauthorized"
        );
    }

    #[tokio::test]
    async fn table_and_row_resource_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), None, None).unwrap());
        let table = serde_json::json!({
            "name": "items",
            "columns": [
                {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null,"compact_raw":false},
                {"name":"title","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Default","compact_raw":false}
            ]
        });
        let created = app
            .clone()
            .oneshot(json_request("POST", "/api/v1/tables", table))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let inserted = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/items/rows",
                serde_json::json!({"id":1,"title":"First"}),
            ))
            .await
            .unwrap();
        assert_eq!(inserted.status(), StatusCode::CREATED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tables/items/rows/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let etag = response.headers()[header::ETAG].clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"]["title"], "First");

        let patched = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/tables/items/rows/1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, etag.clone())
                    .body(Body::from(r#"{"title":"Second"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(patched.status(), StatusCode::OK);
        let stale = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/tables/items/rows/1")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::IF_MATCH, etag)
                    .body(Body::from(r#"{"title":"Third"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    }

    #[tokio::test]
    async fn query_returns_a_reusable_search_after_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), None, None).unwrap());
        let table = serde_json::json!({
            "name": "pages",
            "columns": [
                {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null,"compact_raw":false},
                {"name":"category","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Raw","compact_raw":false}
            ]
        });
        app.clone()
            .oneshot(json_request("POST", "/api/v1/tables", table))
            .await
            .unwrap();
        for id in 1..=3 {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/api/v1/tables/pages/rows",
                    serde_json::json!({"id":id,"category":"same"}),
                ))
                .await
                .unwrap();
        }

        let query = serde_json::json!({
            "order_by":[{"column":"category","descending":false}],
            "limit":1
        });
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/pages/query",
                query.clone(),
            ))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let first: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(first["data"][0]["id"], 1);
        assert_eq!(
            first["meta"]["next_search_after"],
            serde_json::json!(["same", 1])
        );

        let mut next = query;
        next["search_after"] = first["meta"]["next_search_after"].clone();
        let response = app
            .oneshot(json_request("POST", "/api/v1/tables/pages/query", next))
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let second: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(second["data"][0]["id"], 2);
    }

    #[tokio::test]
    async fn score_explanation_endpoint_returns_tantivy_tree() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), None, None).unwrap());
        let table = serde_json::json!({
            "name":"articles",
            "columns":[
                {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null,"compact_raw":false},
                {"name":"title","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Default","compact_raw":false}
            ]
        });
        app.clone()
            .oneshot(json_request("POST", "/api/v1/tables", table))
            .await
            .unwrap();
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/articles/rows",
                serde_json::json!({"id":1,"title":"wireless headphones"}),
            ))
            .await
            .unwrap();

        let response = app
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/articles/rows/1/explain-score",
                serde_json::json!({
                    "filter":{"kind":"search","fields":["title"],"query":"headphones"}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["data"]["score"].as_f64().unwrap() > 0.0);
        assert!(body["data"]["explanation"]["details"].is_array());
    }

    #[tokio::test]
    async fn facet_endpoint_can_exclude_its_own_filter() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), None, None).unwrap());
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables",
                serde_json::json!({
                    "name":"items",
                    "columns":[
                        {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null},
                        {"name":"category","data_type":"Facet","primary_key":false,"nullable":false,"analyzer":null},
                        {"name":"status","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Raw"}
                    ]
                }),
            ))
            .await
            .unwrap();
        for (id, category, status) in [
            (1, "/products/audio", "open"),
            (2, "/products/books", "open"),
            (3, "/products/games", "closed"),
        ] {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/api/v1/tables/items/rows",
                    serde_json::json!({"id":id,"category":category,"status":status}),
                ))
                .await
                .unwrap();
        }
        let response = app
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/items/facets/category",
                serde_json::json!({
                    "root":"/products",
                    "exclude_own_filter":true,
                    "filter":{"kind":"all","filters":[
                        {"kind":"compare","column":"category","operator":"equal","value":"/products/audio"},
                        {"kind":"compare","column":"status","operator":"equal","value":"open"}
                    ]}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
        assert_eq!(body["data"][0]["path"], "/products/audio");
        assert_eq!(body["data"][1]["path"], "/products/books");
    }

    #[tokio::test]
    async fn distributed_aggregation_endpoints_round_trip_opaque_fruits() {
        let directory = tempfile::tempdir().unwrap();
        let app = router(AppState::open(directory.path(), None, None).unwrap());
        app.clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables",
                serde_json::json!({
                    "name":"events",
                    "columns":[
                        {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null,"compact_raw":false},
                        {"name":"category","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Raw","compact_raw":false}
                    ]
                }),
            ))
            .await
            .unwrap();
        for (id, category) in [(1, "a"), (2, "a"), (3, "b")] {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/api/v1/tables/events/rows",
                    serde_json::json!({"id":id,"category":category}),
                ))
                .await
                .unwrap();
        }
        let aggregations = serde_json::json!({
            "categories":{"kind":"terms","column":"category","size":10}
        });
        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/events/aggregate-intermediate",
                serde_json::json!({"limit":0,"aggregations":aggregations}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let intermediate: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(intermediate["data"]["format"], "tantivy-bincode-v1");

        let response = app
            .oneshot(json_request(
                "POST",
                "/api/v1/tables/events/aggregate-merge",
                serde_json::json!({
                    "aggregations":aggregations,
                    "payloads_hex":[intermediate["data"]["payload_hex"]]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["data"]["categories"]["buckets"][0]["doc_count"], 2);
    }

    fn json_request(method: &str, uri: &str, value: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap()
    }
}

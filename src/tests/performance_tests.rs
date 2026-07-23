use super::geo_support::{VILNIUS, geo_database, geo_sort};
use super::*;

#[test]
fn field_sort_with_score_projection_uses_bounded_scored_collector() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        vec![
            Projection::Column {
                column: "id".into(),
                alias: None,
            },
            Projection::Score { alias: None },
        ],
        Some(Filter::Search {
            fields: vec!["title".into()],
            query: "wireless".into(),
        }),
        vec![Sort {
            column: "price".into(),
            json_path: None,
            json_type: None,
            descending: false,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );
    request.limit = 1;

    let plan = database.explain(&request).unwrap();
    let collector = plan
        .columns
        .iter()
        .position(|column| column == "collector")
        .unwrap();
    assert_eq!(plan.rows[0][collector], json!("scored_fast_field_top_docs"));

    let result = database.read(request.clone()).unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], json!(3));
    assert!(result.rows[0][1].as_f64().unwrap() > 0.0);

    request.min_score = Some(f32::MAX);
    assert!(database.read(request).unwrap().rows.is_empty());
}

#[test]
fn typed_group_orders_are_pushed_down_without_changing_results() {
    let (_directory, mut database) = database();
    products(&mut database);
    let grouped = |order_column: &str, descending: bool| ReadRequest {
        table: "products".into(),
        projection: vec![
            Projection::Column {
                column: "category".into(),
                alias: None,
            },
            Projection::Aggregate {
                function: Aggregate::Count,
                column: None,
                alias: "count".into(),
            },
            Projection::Aggregate {
                function: Aggregate::Average,
                column: Some("price".into()),
                alias: "average".into(),
            },
        ],
        filter: None,
        group_by: vec!["category".into()],
        order_by: vec![Sort {
            column: order_column.into(),
            json_path: None,
            json_type: None,
            descending,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
        limit: 1,
        offset: 0,
        search_after: None,
        min_score: None,
    };

    let by_count = database.read(grouped("count", false)).unwrap();
    assert_eq!(
        by_count.rows,
        vec![vec![json!("computers"), json!(1), json!(45.0)]]
    );

    let by_key = database.read(grouped("category", true)).unwrap();
    assert_eq!(by_key.rows[0][0], json!("computers"));

    let by_average = database.read(grouped("average", false)).unwrap();
    assert_eq!(by_average.rows[0][0], json!("computers"));
}

#[test]
fn geo_distance_sort_uses_block_top_k() {
    let (_directory, mut database) = geo_database();
    let request = read(
        "places",
        columns(&["id"]),
        None,
        vec![geo_sort("location", VILNIUS, GeoDistanceMode::Min)],
    );
    let plan = database.explain(&request).unwrap();
    let collector = plan
        .columns
        .iter()
        .position(|column| column == "collector")
        .unwrap();
    assert_eq!(plan.rows[0][collector], json!("block_fast_field_top_docs"));
    assert_eq!(database.read(request).unwrap().rows[0][0], json!(1));
}

#[test]
fn row_count_fast_paths_preserve_nullable_value_count() {
    let (_directory, mut database) = database();
    products(&mut database);
    let count = |column: Option<&str>, alias: &str, filter| ReadRequest {
        table: "products".into(),
        projection: vec![Projection::Aggregate {
            function: Aggregate::Count,
            column: column.map(str::to_owned),
            alias: alias.into(),
        }],
        filter,
        group_by: vec![],
        order_by: vec![],
        limit: 10,
        offset: 0,
        search_after: None,
        min_score: None,
    };

    let metadata_count = count(None, "rows", None);
    assert_eq!(
        database.explain(&metadata_count).unwrap().rows[0][2],
        json!("metadata_count")
    );
    assert_eq!(
        database.read(metadata_count).unwrap().rows,
        vec![vec![json!(3)]]
    );
    assert_eq!(
        database
            .read(count(
                Some("id"),
                "active_rows",
                Some(equal("active", json!(true))),
            ))
            .unwrap()
            .rows,
        vec![vec![json!(2)]]
    );
    assert_eq!(
        database
            .read(count(Some("note"), "notes", None))
            .unwrap()
            .rows,
        vec![vec![json!(1)]]
    );
}

#[test]
fn unsorted_structural_reads_use_bounded_document_order() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        columns(&["id"]),
        Some(Filter::Compare {
            column: "id".into(),
            operator: Comparison::GreaterOrEqual,
            value: json!(1),
        }),
        vec![],
    );
    request.limit = 1;
    request.offset = 1;

    let plan = database.explain(&request).unwrap();
    assert_eq!(plan.rows[0][2], json!("doc_order_top_docs"));
    let mut full_request = request.clone();
    full_request.limit = 3;
    full_request.offset = 0;
    let full = database.read(full_request).unwrap();
    assert_eq!(
        database.read(request).unwrap().rows,
        vec![full.rows[1].clone()]
    );
}

#[test]
fn materialized_fallback_sort_is_bounded_and_ordered() {
    let (_directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "json_sort".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("payload", ColumnType::Json, false, true, None),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "json_sort",
            &[
                vec![json!(1), json!({"rank": 3})],
                vec![json!(2), Value::Null],
                vec![json!(3), json!({"rank": 1})],
                vec![json!(4), json!({"rank": 2})],
            ],
        )
        .unwrap();
    let mut request = read(
        "json_sort",
        columns(&["id", "payload"]),
        None,
        vec![Sort {
            column: "payload".into(),
            json_path: None,
            json_type: None,
            descending: false,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );
    request.limit = 2;
    request.offset = 1;

    let plan = database.explain(&request).unwrap();
    assert_eq!(plan.rows[0][2], json!("materialized_top_docs"));
    assert_eq!(
        database.read(request).unwrap().rows,
        vec![
            vec![json!(4), json!({"rank": 2})],
            vec![json!(1), json!({"rank": 3})]
        ]
    );
}

#[test]
fn multiple_segments_share_workers_for_sort_and_aggregation() {
    let (_directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "parallel_segments".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("value", ColumnType::Integer, false, false, None),
                test_column("kind", ColumnType::Text, false, false, Some(Analyzer::Raw)),
            ],
        })
        .unwrap();
    for batch in 0..2 {
        let rows = (0..150)
            .map(|row| {
                let id = batch * 150 + row;
                vec![
                    json!(id),
                    json!((id * 47) % 101),
                    json!(format!("kind-{}", id % 5)),
                ]
            })
            .collect::<Vec<_>>();
        database
            .bulk_insert_json("parallel_segments", &rows)
            .unwrap();
    }
    let options = |worker_threads| SearchOptions {
        worker_threads,
        aggregation_cache_entries: 0,
        warmup_fast_fields: false,
    };
    let mut request = read(
        "parallel_segments",
        columns(&["id", "value"]),
        None,
        vec![Sort {
            column: "value".into(),
            json_path: None,
            json_type: None,
            descending: true,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );
    request.limit = 17;
    request.offset = 9;
    let serial = database
        .search_service_with_options(options(1))
        .unwrap()
        .read(request.clone())
        .unwrap();
    let parallel = database.search_service_with_options(options(4)).unwrap();
    assert_eq!(parallel.read(request.clone()).unwrap().rows, serial.rows);
    let profile = parallel.profile(request).unwrap();
    assert!(profile["segments"].as_u64().unwrap() > 1);
    assert_eq!(profile["sort_strategy"], json!("segment_ranges"));
    assert_eq!(profile["sort_workers"], json!(4));

    let aggregations = BTreeMap::from([(
        "kinds".into(),
        Aggregation::Terms {
            column: "kind".into(),
            size: 10,
            segment_size: None,
            min_doc_count: None,
            missing: None,
            order: None,
            aggregations: BTreeMap::new(),
        },
    )]);
    let serial = database
        .search_service_with_options(options(1))
        .unwrap()
        .aggregate("parallel_segments", None, aggregations.clone())
        .unwrap();
    assert_eq!(
        parallel
            .aggregate("parallel_segments", None, aggregations.clone())
            .unwrap(),
        serial
    );
    let profile = parallel
        .profile_with_aggregations(
            ReadRequest {
                table: "parallel_segments".into(),
                projection: vec![],
                filter: None,
                group_by: vec![],
                order_by: vec![],
                limit: 0,
                offset: 0,
                search_after: None,
                min_score: None,
            },
            aggregations,
        )
        .unwrap();
    assert_eq!(profile["aggregation_strategy"], json!("segment_ranges"));
    assert_eq!(profile["aggregation_workers"], json!(4));
}

#[test]
fn scored_string_sort_materializes_only_cross_segment_candidates() {
    let (_directory, mut database) = database();
    database.create_table_def(products_table()).unwrap();
    database
        .bulk_insert_json(
            "products",
            &[
                vec![
                    json!(1),
                    json!("Wireless alpha"),
                    json!("zeta"),
                    json!(10.0),
                    json!(true),
                    json!("2026-07-20T10:00:00Z"),
                    Value::Null,
                ],
                vec![
                    json!(2),
                    json!("Wireless beta"),
                    json!("alpha"),
                    json!(20.0),
                    json!(true),
                    json!("2026-07-20T10:00:00Z"),
                    Value::Null,
                ],
            ],
        )
        .unwrap();
    database
        .bulk_insert_json(
            "products",
            &[
                vec![
                    json!(3),
                    json!("Wireless gamma"),
                    json!("beta"),
                    json!(30.0),
                    json!(true),
                    json!("2026-07-20T10:00:00Z"),
                    Value::Null,
                ],
                vec![
                    json!(4),
                    json!("Wireless delta"),
                    json!("alpha"),
                    json!(40.0),
                    json!(true),
                    json!("2026-07-20T10:00:00Z"),
                    Value::Null,
                ],
            ],
        )
        .unwrap();
    let request = read(
        "products",
        vec![
            Projection::Column {
                column: "id".into(),
                alias: None,
            },
            Projection::Score { alias: None },
        ],
        Some(Filter::Search {
            fields: vec!["title".into()],
            query: "wireless".into(),
        }),
        vec![Sort {
            column: "category".into(),
            json_path: None,
            json_type: None,
            descending: false,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );

    let result = database.read(request).unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        vec![json!(2), json!(4), json!(3), json!(1)]
    );
    assert!(result.rows.iter().all(|row| row[1].as_f64().unwrap() > 0.0));
}

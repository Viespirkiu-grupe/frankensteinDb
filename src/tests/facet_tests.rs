use super::*;

fn facet_database() -> (tempfile::TempDir, Database) {
    let (directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "catalog".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("category", ColumnType::Facet, false, false, None),
                test_column(
                    "status",
                    ColumnType::Text,
                    false,
                    false,
                    Some(Analyzer::Raw),
                ),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "catalog",
            &[
                vec![json!(1), json!("/products/audio"), json!("open")],
                vec![json!(2), json!("/products/audio"), json!("closed")],
                vec![json!(3), json!("/products/books"), json!("open")],
                vec![json!(4), json!("/products/games"), json!("closed")],
            ],
        )
        .unwrap();
    (directory, database)
}

fn selected_audio_and_open() -> Filter {
    Filter::All {
        filters: vec![
            Filter::In {
                column: "category".into(),
                values: vec![json!("/products/audio")],
            },
            Filter::Compare {
                column: "status".into(),
                operator: Comparison::Equal,
                value: json!("open"),
            },
        ],
    }
}

#[test]
fn exclude_own_filter_keeps_other_filter_dimensions() {
    let (_directory, database) = facet_database();
    let search = database.search_service().unwrap();
    let filter = selected_audio_and_open();
    let selected = search
        .facets("catalog", "category", "/products", 10, Some(&filter))
        .unwrap();
    assert_eq!(selected, json!([{"path":"/products/audio","count":1}]));

    let alternatives = search
        .facets_excluding_own_filter("catalog", "category", "/products", 10, Some(&filter))
        .unwrap();
    assert_eq!(
        alternatives,
        json!([
            {"path":"/products/audio","count":1},
            {"path":"/products/books","count":1}
        ])
    );
}

#[test]
fn exclude_own_filter_removes_a_negated_facet_constraint() {
    let (_directory, database) = facet_database();
    let filter = Filter::Not {
        filter: Box::new(Filter::Compare {
            column: "category".into(),
            operator: Comparison::Equal,
            value: json!("/products/audio"),
        }),
    };
    let result = database
        .search_service()
        .unwrap()
        .facets_excluding_own_filter("catalog", "category", "/products", 10, Some(&filter))
        .unwrap();
    assert_eq!(
        result,
        json!([
            {"path":"/products/audio","count":2},
            {"path":"/products/books","count":1},
            {"path":"/products/games","count":1}
        ])
    );
}

#[test]
fn filter_aggregations_accept_typed_date_and_time_bounds() {
    let (directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "events".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("day", ColumnType::Date, false, false, None),
                test_column("observed_at", ColumnType::DateTime, false, false, None),
                test_column("edited_at", ColumnType::Timestamp, false, false, None),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "events",
            &[
                vec![
                    json!(1),
                    json!("2024-01-01"),
                    json!("2024-01-01T10:00:00Z"),
                    json!("2024-01-01T10:00:00.000"),
                ],
                vec![
                    json!(2),
                    json!("2024-01-02"),
                    json!("2024-01-02T10:00:00Z"),
                    json!("2024-01-02T10:00:00.000"),
                ],
            ],
        )
        .unwrap();

    let filtered = |filter| Aggregation::Filter {
        filter,
        aggregations: BTreeMap::new(),
    };
    let aggregations = BTreeMap::from([
        (
            "days".into(),
            filtered(Filter::Compare {
                column: "day".into(),
                operator: Comparison::GreaterOrEqual,
                value: json!("2024-01-02"),
            }),
        ),
        (
            "datetimes".into(),
            filtered(Filter::Between {
                column: "observed_at".into(),
                lower: json!("2024-01-01T00:00:00Z"),
                upper: json!("2024-01-01T23:59:59Z"),
            }),
        ),
        (
            "timestamps".into(),
            filtered(Filter::Compare {
                column: "edited_at".into(),
                operator: Comparison::Greater,
                value: json!("2024-01-01T12:00:00.000"),
            }),
        ),
    ]);
    let result = database
        .search_service()
        .unwrap()
        .aggregate("events", None, aggregations)
        .unwrap();

    assert_eq!(result["days"]["doc_count"], json!(1));
    assert_eq!(result["datetimes"]["doc_count"], json!(1));
    assert_eq!(result["timestamps"]["doc_count"], json!(1));
    drop(directory);
}

#[test]
fn filter_aggregations_use_typed_prefix_in_and_null_queries() {
    let (_directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "filter_values".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column(
                    "codes",
                    ColumnType::TextArray,
                    false,
                    false,
                    Some(Analyzer::Raw),
                ),
                test_column("kind", ColumnType::Text, false, true, Some(Analyzer::Raw)),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "filter_values",
            &[
                vec![json!(1), json!(["15800000-6"]), json!("SP")],
                vec![json!(2), json!(["45230000-8"]), json!("MVP")],
                vec![json!(3), json!(["45233100-0"]), Value::Null],
            ],
        )
        .unwrap();

    let filtered = |filter| Aggregation::Filter {
        filter,
        aggregations: BTreeMap::new(),
    };
    let aggregations = BTreeMap::from([
        (
            "code_prefix".into(),
            filtered(Filter::Prefix {
                column: "codes".into(),
                value: "4523".into(),
            }),
        ),
        (
            "kind_prefix".into(),
            filtered(Filter::Prefix {
                column: "kind".into(),
                value: "MV".into(),
            }),
        ),
        (
            "selected_codes".into(),
            filtered(Filter::In {
                column: "codes".into(),
                values: vec![json!("15800000-6"), json!("45230000-8")],
            }),
        ),
        (
            "missing_kind".into(),
            filtered(Filter::IsNull {
                column: "kind".into(),
                negated: false,
            }),
        ),
    ]);
    let profile_aggregations = aggregations.clone();
    let result = database
        .search_service()
        .unwrap()
        .aggregate("filter_values", None, aggregations)
        .unwrap();

    assert_eq!(result["code_prefix"]["doc_count"], json!(2));
    assert_eq!(result["kind_prefix"]["doc_count"], json!(1));
    assert_eq!(result["selected_codes"]["doc_count"], json!(2));
    assert_eq!(result["missing_kind"]["doc_count"], json!(1));

    let profile = database
        .search_service()
        .unwrap()
        .profile_with_aggregations(
            ReadRequest {
                table: "filter_values".into(),
                projection: vec![],
                filter: None,
                group_by: vec![],
                order_by: vec![],
                limit: 0,
                offset: 0,
                search_after: None,
                min_score: None,
            },
            profile_aggregations,
        )
        .unwrap();
    assert_eq!(profile["profiled_aggregations"], json!(4));
    assert_eq!(profile["aggregation_cache_bypassed"], json!(true));
    assert!(profile["timing_ms"]["aggregations"].is_number());
    assert_eq!(profile["returned_rows"], json!(0));
}

#[test]
fn aggregation_cache_is_invalidated_when_a_new_generation_is_published() {
    let (_directory, mut database) = facet_database();
    let search = database
        .search_service_with_options(SearchOptions {
            worker_threads: 2,
            aggregation_cache_entries: 8,
            warmup_fast_fields: false,
        })
        .unwrap();
    let aggregations = BTreeMap::from([(
        "matching".into(),
        Aggregation::Filter {
            filter: Filter::Compare {
                column: "id".into(),
                operator: Comparison::GreaterOrEqual,
                value: json!(0),
            },
            aggregations: BTreeMap::new(),
        },
    )]);

    let first = search
        .aggregate("catalog", None, aggregations.clone())
        .unwrap();
    assert_eq!(first["matching"]["doc_count"], json!(4));
    assert_eq!(
        search
            .aggregate("catalog", None, aggregations.clone())
            .unwrap(),
        first
    );

    database
        .bulk_insert_json(
            "catalog",
            &[vec![json!(5), json!("/products/books"), json!("open")]],
        )
        .unwrap();
    search.publish_catalog(database.tables().unwrap()).unwrap();
    let second = search.aggregate("catalog", None, aggregations).unwrap();
    assert_eq!(second["matching"]["doc_count"], json!(5));

    let profile = search
        .profile(ReadRequest {
            table: "catalog".into(),
            projection: vec![],
            filter: None,
            group_by: vec![],
            order_by: vec![],
            limit: 0,
            offset: 0,
            search_after: None,
            min_score: None,
        })
        .unwrap();
    assert_eq!(profile["search_worker_threads"], json!(2));
}

#[test]
fn intra_segment_ranges_match_single_worker_aggregation_results() {
    let (_directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "range_aggs".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("value", ColumnType::Integer, false, false, None),
                test_column("kind", ColumnType::Text, false, false, Some(Analyzer::Raw)),
            ],
        })
        .unwrap();
    let rows = (0..350)
        .map(|id| vec![json!(id), json!(id % 17), json!(format!("kind-{}", id % 5))])
        .collect::<Vec<_>>();
    database.bulk_insert_json("range_aggs", &rows).unwrap();
    database
        .optimize_table_with_options(
            "range_aggs",
            OptimizeOptions {
                target_segments: 1,
                merge_threads: 1,
            },
        )
        .unwrap();
    let latest = Aggregation::TopHits {
        size: 3,
        sort: vec![Sort {
            column: "value".into(),
            json_path: None,
            json_type: None,
            descending: true,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
        columns: vec!["id".into(), "value".into()],
    };
    let aggregations = BTreeMap::from([
        (
            "kinds".into(),
            Aggregation::Terms {
                column: "kind".into(),
                size: 10,
                segment_size: Some(20),
                min_doc_count: Some(1),
                missing: None,
                order: None,
                aggregations: BTreeMap::from([("latest".into(), latest)]),
            },
        ),
        (
            "values".into(),
            Aggregation::Range {
                column: "value".into(),
                ranges: vec![
                    AggregationRange {
                        key: Some("low".into()),
                        from: None,
                        to: Some(json!(8)),
                    },
                    AggregationRange {
                        key: Some("high".into()),
                        from: Some(json!(8)),
                        to: None,
                    },
                ],
                keyed: true,
                aggregations: BTreeMap::new(),
            },
        ),
    ]);
    let filter = Filter::Compare {
        column: "id".into(),
        operator: Comparison::GreaterOrEqual,
        value: json!(37),
    };
    let options = |worker_threads| SearchOptions {
        worker_threads,
        aggregation_cache_entries: 0,
        warmup_fast_fields: false,
    };
    let serial = database
        .search_service_with_options(options(1))
        .unwrap()
        .aggregate("range_aggs", Some(&filter), aggregations.clone())
        .unwrap();
    let parallel_search = database.search_service_with_options(options(4)).unwrap();
    let parallel = parallel_search
        .aggregate("range_aggs", Some(&filter), aggregations.clone())
        .unwrap();
    assert_eq!(parallel, serial);
    let profile = parallel_search
        .profile_with_aggregations(
            ReadRequest {
                table: "range_aggs".into(),
                projection: vec![],
                filter: Some(filter),
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
    assert_eq!(
        profile["aggregation_strategy"],
        json!("intra_segment_ranges")
    );
    assert_eq!(profile["aggregation_workers"], json!(4));
}

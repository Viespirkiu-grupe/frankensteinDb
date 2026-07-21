use std::collections::BTreeMap;

use super::*;

fn json_database() -> (TempDir, Database) {
    let (directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "events".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column("metadata", ColumnType::Json, false, false, None),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "events",
            &[
                vec![json!(1), json!({"category":"a","rank":3,"price":12.5,"active":true,"at":"2026-01-10T12:00:00Z"})],
                vec![json!(2), json!({"category":"b","rank":1,"price":25.0,"active":false,"at":"2026-02-10T12:00:00Z"})],
                vec![json!(3), json!({"rank":2,"active":true,"at":"2026-03-10T12:00:00Z"})],
            ],
        )
        .unwrap();
    (directory, database)
}

fn path(path: &str, data_type: JsonPathType) -> JsonPath {
    JsonPath {
        column: "metadata".into(),
        path: path.into(),
        data_type,
    }
}

#[test]
fn typed_json_filters_exist_ranges_and_sort_use_dynamic_fast_fields() {
    let (_directory, mut database) = json_database();
    let request = read(
        "events",
        columns(&["id"]),
        Some(Filter::All {
            filters: vec![
                Filter::JsonBetween {
                    column: "metadata".into(),
                    path: "rank".into(),
                    data_type: JsonPathType::I64,
                    lower: json!(1),
                    upper: json!(3),
                },
                Filter::JsonExists {
                    column: "metadata".into(),
                    path: "price".into(),
                    data_type: Some(JsonPathType::F64),
                    negated: false,
                },
            ],
        }),
        vec![Sort {
            column: "metadata".into(),
            json_path: Some("rank".into()),
            json_type: Some(JsonPathType::I64),
            descending: false,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
    );
    assert_eq!(
        database.read(request).unwrap().rows,
        vec![vec![json!(2)], vec![json!(1)]]
    );

    let datetime = database
        .read(read(
            "events",
            columns(&["id"]),
            Some(Filter::JsonCompare {
                column: "metadata".into(),
                path: "at".into(),
                data_type: JsonPathType::DateTime,
                operator: Comparison::Greater,
                value: json!("2026-02-01T00:00:00Z"),
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(datetime.rows.len(), 2);

    let error = database
        .read(read(
            "events",
            columns(&["id"]),
            Some(Filter::JsonCompare {
                column: "metadata".into(),
                path: "rank".into(),
                data_type: JsonPathType::String,
                operator: Comparison::Equal,
                value: json!("1"),
            }),
            vec![],
        ))
        .unwrap_err();
    assert!(error.to_string().contains("dynamic type"));
}

#[test]
fn json_path_aggregations_support_missing_bounds_order_and_metrics() {
    let (_directory, database) = json_database();
    let search = database.search_service().unwrap();
    let aggregations = BTreeMap::from([
        (
            "categories".into(),
            Aggregation::JsonTerms {
                target: path("category", JsonPathType::String),
                size: 10,
                missing: Some(json!("unknown")),
                order: Some(BucketOrder {
                    target: "_key".into(),
                    descending: true,
                }),
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "prices".into(),
            Aggregation::JsonHistogram {
                target: path("price", JsonPathType::F64),
                interval: 10.0,
                min_doc_count: 0,
                hard_bounds: Some(HistogramBounds {
                    min: json!(0),
                    max: json!(30),
                }),
                extended_bounds: Some(HistogramBounds {
                    min: json!(0),
                    max: json!(30),
                }),
                keyed: true,
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "average_price".into(),
            Aggregation::Metric {
                function: Metric::Average,
                column: None,
                json_path: Some(path("price", JsonPathType::F64)),
                percents: None,
                missing: Some(json!(0)),
            },
        ),
        (
            "active".into(),
            Aggregation::Filter {
                filter: Filter::JsonCompare {
                    column: "metadata".into(),
                    path: "active".into(),
                    data_type: JsonPathType::Bool,
                    operator: Comparison::Equal,
                    value: json!(true),
                },
                aggregations: BTreeMap::new(),
            },
        ),
    ]);
    let result = search.aggregate("events", None, aggregations).unwrap();
    assert_eq!(result["categories"]["buckets"].as_array().unwrap().len(), 3);
    assert!(result["prices"]["buckets"].is_object());
    assert_eq!(result["average_price"]["value"], json!(12.5));
    assert_eq!(result["active"]["doc_count"], json!(2));
}

#[test]
fn keyed_ranges_calendar_composites_and_distributed_merge_work() {
    let (_directory, database) = json_database();
    let search = database.search_service().unwrap();
    let aggregations = BTreeMap::from([
        (
            "bands".into(),
            Aggregation::JsonRange {
                target: path("rank", JsonPathType::I64),
                ranges: vec![AggregationRange {
                    key: Some("low".into()),
                    from: Some(json!(0)),
                    to: Some(json!(3)),
                }],
                keyed: true,
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "months".into(),
            Aggregation::Composite {
                sources: vec![CompositeSource::JsonTerms {
                    name: "category".into(),
                    target: path("category", JsonPathType::String),
                    descending: false,
                    missing_bucket: true,
                    missing_order: MissingOrder::First,
                }],
                size: 10,
                after: BTreeMap::new(),
                aggregations: BTreeMap::new(),
            },
        ),
    ]);
    let result = search
        .aggregate("events", None, aggregations.clone())
        .unwrap();
    assert!(result["bands"]["buckets"].is_object());
    assert_eq!(result["months"]["buckets"].as_array().unwrap().len(), 3);

    let payload = search
        .aggregate_intermediate("events", None, aggregations.clone())
        .unwrap();
    let merged = search
        .merge_aggregation_intermediates(
            "events",
            aggregations.clone(),
            std::slice::from_ref(&payload),
        )
        .unwrap();
    assert_eq!(merged, result);

    let incompatible = BTreeMap::from([(
        "different".into(),
        Aggregation::Metric {
            function: Metric::Count,
            column: Some("id".into()),
            json_path: None,
            percents: None,
            missing: None,
        },
    )]);
    let error = search
        .merge_aggregation_intermediates("events", incompatible, &[payload])
        .unwrap_err();
    assert!(error.to_string().contains("does not match"));
}

#[test]
fn composite_calendar_interval_and_missing_order_compile() {
    let (_directory, mut database) = database();
    products(&mut database);
    let bounds = HistogramBounds {
        min: json!("2026-07-18T00:00:00Z"),
        max: json!("2026-07-20T23:59:59Z"),
    };
    let aggregations = BTreeMap::from([
        (
            "months".into(),
            Aggregation::Composite {
                sources: vec![CompositeSource::DateHistogram {
                    name: "month".into(),
                    column: "created_at".into(),
                    fixed_interval: None,
                    calendar_interval: Some(CalendarInterval::Month),
                    descending: true,
                    missing_bucket: true,
                    missing_order: MissingOrder::Last,
                }],
                size: 10,
                after: BTreeMap::new(),
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "days".into(),
            Aggregation::DateHistogram {
                column: "created_at".into(),
                fixed_interval: "1d".into(),
                offset: Some("1h".into()),
                min_doc_count: 0,
                hard_bounds: Some(bounds.clone()),
                extended_bounds: Some(bounds),
                keyed: true,
                aggregations: BTreeMap::new(),
            },
        ),
    ]);
    let result = database
        .search_service()
        .unwrap()
        .aggregate("products", None, aggregations)
        .unwrap();
    assert_eq!(result["months"]["buckets"].as_array().unwrap().len(), 1);
    assert!(result["days"]["buckets"].is_object());
}

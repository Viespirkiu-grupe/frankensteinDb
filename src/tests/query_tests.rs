use super::*;

#[test]
fn typed_reads_search_filter_sort_and_paginate_through_tantivy() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        columns(&["id", "title"]),
        Some(Filter::All {
            filters: vec![
                Filter::Search {
                    fields: vec!["title".into()],
                    query: "headphone".into(),
                },
                equal("active", json!(true)),
            ],
        }),
        vec![Sort {
            column: "id".into(),
            json_path: None,
            json_type: None,
            descending: false,
        }],
    );
    request.limit = 1;
    assert_eq!(
        database.read(request.clone()).unwrap().rows,
        vec![vec![json!(1), json!("Wireless headphones")]]
    );
    request.offset = 1;
    assert_eq!(database.read(request).unwrap().rows[0][0], json!(2));
}

#[test]
fn large_in_filter_uses_set_semantics_and_deduplicates_terms() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut values = (1_000..6_000).map(Value::from).collect::<Vec<_>>();
    values.extend([json!(3), json!(1), json!(3)]);

    let result = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::In {
                column: "id".into(),
                values,
            }),
            vec![Sort {
                column: "id".into(),
                json_path: None,
                json_type: None,
                descending: false,
            }],
        ))
        .unwrap();

    assert_eq!(result.rows, vec![vec![json!(1)], vec![json!(3)]]);
}

#[test]
fn search_after_pages_by_sort_values_and_primary_key_tie_breaker() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        columns(&["id", "category"]),
        None,
        vec![Sort {
            column: "category".into(),
            json_path: None,
            json_type: None,
            descending: false,
        }],
    );
    request.limit = 1;

    let first = database.read(request.clone()).unwrap();
    assert_eq!(first.rows, vec![vec![json!(1), json!("audio")]]);
    assert_eq!(
        first.next_search_after,
        Some(vec![json!("audio"), json!(1)])
    );

    request.search_after = first.next_search_after;
    let second = database.read(request.clone()).unwrap();
    assert_eq!(second.rows, vec![vec![json!(2), json!("audio")]]);
    assert_eq!(
        second.next_search_after,
        Some(vec![json!("audio"), json!(2)])
    );

    request.search_after = second.next_search_after;
    let third = database.read(request).unwrap();
    assert_eq!(third.rows, vec![vec![json!(3), json!("computers")]]);
    assert_eq!(third.next_search_after, None);

    let mut descending = read(
        "products",
        columns(&["id", "category"]),
        None,
        vec![Sort {
            column: "category".into(),
            json_path: None,
            json_type: None,
            descending: true,
        }],
    );
    descending.limit = 1;
    let first = database.read(descending.clone()).unwrap();
    assert_eq!(first.rows[0][0], json!(3));
    descending.search_after = first.next_search_after;
    let second = database.read(descending).unwrap();
    assert_eq!(second.rows[0][0], json!(1));
}

#[test]
fn typed_ranges_nulls_and_score_are_supported() {
    let (_directory, mut database) = database();
    products(&mut database);
    let result = database
        .read(read(
            "products",
            vec![
                Projection::Column {
                    column: "id".into(),
                    alias: None,
                },
                Projection::Score { alias: None },
            ],
            Some(Filter::Search {
                fields: vec![],
                query: "wireless".into(),
            }),
            vec![Sort {
                column: "_score".into(),
                json_path: None,
                json_type: None,
                descending: true,
            }],
        ))
        .unwrap();
    assert_eq!(result.rows.len(), 2);

    let result = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::All {
                filters: vec![
                    Filter::Between {
                        column: "price".into(),
                        lower: json!(40),
                        upper: json!(80),
                    },
                    Filter::IsNull {
                        column: "note".into(),
                        negated: false,
                    },
                ],
            }),
            vec![Sort {
                column: "id".into(),
                json_path: None,
                json_type: None,
                descending: false,
            }],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(1)], vec![json!(3)]]);
}

#[test]
fn score_sort_keeps_the_bounded_top_docs_collector() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mut request = read(
        "products",
        columns(&["id"]),
        Some(Filter::Search {
            fields: vec!["title".into()],
            query: "wireless".into(),
        }),
        vec![Sort {
            column: "_score".into(),
            json_path: None,
            json_type: None,
            descending: true,
        }],
    );
    request.limit = 20;

    let plan = database.explain(&request).unwrap();
    let collector = plan
        .columns
        .iter()
        .position(|name| name == "collector")
        .unwrap();
    assert_eq!(plan.rows[0][collector], json!("score_top_docs"));
    assert_eq!(database.read(request).unwrap().rows.len(), 2);
}

#[test]
fn typed_native_aggregations_cover_metrics_and_groups() {
    let (_directory, mut database) = database();
    products(&mut database);
    let result = database
        .read(ReadRequest {
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
                column: "count".into(),
                json_path: None,
                json_type: None,
                descending: true,
            }],
            limit: 10,
            offset: 0,
            search_after: None,
            min_score: None,
        })
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], json!("audio"));
    assert_eq!(result.rows[0][1], json!(2));
}

#[test]
fn arrays_are_filterable_and_returned_as_arrays() {
    let (_directory, mut database) = database();
    let table = TableDef {
        name: "arrays".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            test_column("id", ColumnType::Integer, true, false, None),
            test_column(
                "tags",
                ColumnType::TextArray,
                false,
                false,
                Some(Analyzer::Raw),
            ),
            test_column("numbers", ColumnType::IntegerArray, false, false, None),
        ],
    };
    database.create_table_def(table).unwrap();
    database
        .bulk_insert_json(
            "arrays",
            &[vec![json!(1), json!(["a", "b"]), json!([1, 2])]],
        )
        .unwrap();
    let result = database
        .read(read(
            "arrays",
            columns(&["tags", "numbers"]),
            Some(equal("numbers", json!(2))),
            vec![],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(["a", "b"]), json!([1, 2])]]);
}

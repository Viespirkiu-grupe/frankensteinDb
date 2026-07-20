use super::*;

#[test]
fn fuzzy_prefix_boost_and_highlight_are_typed() {
    let (_directory, mut database) = database();
    products(&mut database);

    let result = database
        .read(ReadRequest {
            projection: vec![
                Projection::Column {
                    column: "id".into(),
                    alias: None,
                },
                Projection::Highlight {
                    column: "title".into(),
                    alias: Some("snippet".into()),
                    fragment_size: 80,
                },
            ],
            filter: Some(Filter::Search {
                fields: vec!["title".into()],
                query: "headphones".into(),
            }),
            ..read("products", vec![], None, vec![])
        })
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert!(
        result.rows[0][1]
            .as_str()
            .unwrap()
            .contains("<b>headphones</b>")
    );

    let fuzzy = database
        .read(ReadRequest {
            filter: Some(Filter::Fuzzy {
                column: "title".into(),
                value: "hedphones".into(),
                distance: 1,
                transposition_cost_one: true,
            }),
            ..read("products", columns(&["id"]), None, vec![])
        })
        .unwrap();
    assert_eq!(fuzzy.rows.len(), 2);

    let boosted = database
        .read(ReadRequest {
            projection: columns(&["id"]),
            filter: Some(Filter::SearchBoosted {
                fields: BTreeMap::from([("title".into(), 4.0)]),
                query: "wireless".into(),
                conjunction_by_default: false,
            }),
            ..read("products", vec![], None, vec![])
        })
        .unwrap();
    assert_eq!(boosted.rows.len(), 2);
}

#[test]
fn regex_phrase_matches_token_patterns_and_respects_positions() {
    let (_directory, mut database) = database();
    products(&mut database);

    let adjacent = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::RegexPhrase {
                column: "title".into(),
                patterns: vec!["wire.*".into(), "head.*".into()],
                slop: 0,
                max_expansions: 128,
            }),
            vec![Sort {
                column: "id".into(),
                json_path: None,
                json_type: None,
                descending: false,
            }],
        ))
        .unwrap();
    assert_eq!(adjacent.rows, vec![vec![json!(1)], vec![json!(2)]]);

    let mut basic_text = test_column(
        "text",
        ColumnType::Text,
        false,
        false,
        Some(Analyzer::Default),
    );
    basic_text.index.record = TextIndexRecord::Basic;
    database
        .create_table_def(TableDef {
            name: "basic_text".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                basic_text,
            ],
        })
        .unwrap();
    let error = database
        .read(read(
            "basic_text",
            columns(&["id"]),
            Some(Filter::RegexPhrase {
                column: "text".into(),
                patterns: vec!["first.*".into(), "second.*".into()],
                slop: 0,
                max_expansions: 128,
            }),
            vec![],
        ))
        .unwrap_err();
    assert!(error.to_string().contains("requires positions"));
}

#[test]
fn regex_matches_analyzed_terms_without_requiring_positions() {
    let (_directory, mut database) = database();
    let mut title = test_column(
        "title",
        ColumnType::Text,
        false,
        false,
        Some(Analyzer::Default),
    );
    title.index.record = TextIndexRecord::Basic;
    database
        .create_table_def(TableDef {
            name: "regex_docs".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                title,
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "regex_docs",
            &[
                vec![json!(1), json!("Wireless headphones")],
                vec![json!(2), json!("Wired mouse")],
                vec![json!(3), json!("Keyboard")],
            ],
        )
        .unwrap();

    let result = database
        .read(read(
            "regex_docs",
            columns(&["id"]),
            Some(Filter::Regex {
                column: "title".into(),
                pattern: "wire.*".into(),
            }),
            vec![Sort {
                column: "id".into(),
                json_path: None,
                json_type: None,
                descending: false,
            }],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(1)], vec![json!(2)]]);

    let error = database
        .read(read(
            "regex_docs",
            columns(&["id"]),
            Some(Filter::Regex {
                column: "title".into(),
                pattern: "(".into(),
            }),
            vec![],
        ))
        .unwrap_err();
    assert!(error.to_string().contains("RegexQueryError"));
}

#[test]
fn unsigned_ip_json_and_hierarchical_facets_round_trip_through_tantivy() {
    let (_directory, mut database) = database();
    let table = TableDef {
        name: "features".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            test_column("id", ColumnType::Integer, true, false, None),
            test_column("counter", ColumnType::Unsigned, false, false, None),
            test_column("address", ColumnType::Ip, false, false, None),
            test_column("metadata", ColumnType::Json, false, false, None),
            test_column("category", ColumnType::Facet, false, false, None),
        ],
    };
    database.create_table_def(table).unwrap();
    database
        .bulk_insert_json(
            "features",
            &[
                vec![
                    json!(1),
                    json!(u64::MAX),
                    json!("127.0.0.1"),
                    json!({"owner":"alice"}),
                    json!("/services/it"),
                ],
                vec![
                    json!(2),
                    json!(7),
                    json!("2001:db8::1"),
                    json!({"owner":"bob"}),
                    json!("/services/legal"),
                ],
            ],
        )
        .unwrap();
    let rows = database
        .read(read("features", vec![], None, vec![]))
        .unwrap();
    let first = rows.rows.iter().find(|row| row[0] == json!(1)).unwrap();
    assert_eq!(first[1], json!(u64::MAX));
    assert_eq!(first[2], json!("127.0.0.1"));
    assert_eq!(first[3], json!({"owner":"alice"}));

    let search = database.search_service().unwrap();
    let facets = search
        .facets("features", "category", "/services", 10, None)
        .unwrap();
    assert_eq!(facets.as_array().unwrap().len(), 2);
}

#[test]
fn range_and_stats_aggregations_use_tantivy_collectors() {
    let (_directory, mut database) = database();
    products(&mut database);
    let search = database.search_service().unwrap();
    let aggregations = BTreeMap::from([
        (
            "prices".into(),
            Aggregation::Range {
                column: "price".into(),
                ranges: vec![AggregationRange {
                    key: Some("cheap".into()),
                    from: None,
                    to: Some(json!(50)),
                }],
                keyed: false,
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "stats".into(),
            Aggregation::Metric {
                function: Metric::Stats,
                column: Some("price".into()),
                json_path: None,
                percents: None,
                missing: None,
            },
        ),
        (
            "page".into(),
            Aggregation::Composite {
                sources: vec![CompositeSource::Terms {
                    name: "category".into(),
                    column: "category".into(),
                    descending: false,
                    missing_bucket: false,
                    missing_order: MissingOrder::Default,
                }],
                size: 1,
                after: BTreeMap::new(),
                aggregations: BTreeMap::new(),
            },
        ),
    ]);
    let result = search.aggregate("products", None, aggregations).unwrap();
    assert!(result.get("prices").is_some());
    assert_eq!(result["stats"]["count"], json!(3));
    assert_eq!(result["page"]["buckets"].as_array().unwrap().len(), 1);
    assert!(result["page"].get("after_key").is_some());
}

#[test]
fn aliases_resolve_the_same_published_generation() {
    let (_directory, mut database) = database();
    products(&mut database);
    database
        .set_table_aliases("products", vec!["catalog".into()])
        .unwrap();
    let search = database.search_service().unwrap();
    let result = search
        .read(read("catalog", columns(&["id"]), None, vec![]))
        .unwrap();
    assert_eq!(result.rows.len(), 3);
}

#[test]
fn every_scalar_family_has_a_tantivy_multivalue_array() {
    let (_directory, mut database) = database();
    let table = TableDef {
        name: "arrays_all".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            test_column("id", ColumnType::Integer, true, false, None),
            test_column("unsigneds", ColumnType::UnsignedArray, false, false, None),
            test_column("reals", ColumnType::RealArray, false, false, None),
            test_column("flags", ColumnType::BooleanArray, false, false, None),
            test_column("dates", ColumnType::DateArray, false, false, None),
            test_column("datetimes", ColumnType::DateTimeArray, false, false, None),
            test_column("timestamps", ColumnType::TimestampArray, false, false, None),
            test_column("blobs", ColumnType::BlobArray, false, false, None),
            test_column("addresses", ColumnType::IpArray, false, false, None),
            test_column("objects", ColumnType::JsonArray, false, false, None),
            test_column("facets", ColumnType::FacetArray, false, false, None),
        ],
    };
    database.create_table_def(table).unwrap();
    database
        .bulk_insert_json(
            "arrays_all",
            &[vec![
                json!(1),
                json!([0, u64::MAX]),
                json!([1.25, 2.5]),
                json!([true, false]),
                json!(["2026-01-01", "2026-12-31"]),
                json!(["2026-01-01T00:00:00Z"]),
                json!(["2026-01-01T00:00:00.000"]),
                json!(["0x00ff", "0xab"]),
                json!(["127.0.0.1", "2001:db8::1"]),
                json!([{"kind":"a"}, {"kind":"b"}]),
                json!(["/services/it", "/services/legal"]),
            ]],
        )
        .unwrap();

    let rows = database
        .read(read("arrays_all", vec![], None, vec![]))
        .unwrap();
    assert_eq!(rows.rows[0][1], json!([0, u64::MAX]));
    assert_eq!(rows.rows[0][7], json!(["0x00ff", "0xab"]));
    assert_eq!(rows.rows[0][9], json!([{"kind":"a"}, {"kind":"b"}]));

    let filtered = database
        .read(read(
            "arrays_all",
            columns(&["id"]),
            Some(Filter::Compare {
                column: "addresses".into(),
                operator: Comparison::Equal,
                value: json!("127.0.0.1"),
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(filtered.rows, vec![vec![json!(1)]]);

    let search = database.search_service().unwrap();
    let facets = search
        .facets("arrays_all", "facets", "/services", 10, None)
        .unwrap();
    assert_eq!(facets.as_array().unwrap().len(), 2);
}

#[test]
fn more_like_this_uses_tantivy_seed_fields_and_excludes_the_seed() {
    let (_directory, mut database) = database();
    products(&mut database);
    let search = database.search_service().unwrap();

    let result = search
        .more_like_this(
            "products",
            json!(1),
            MoreLikeThisOptions {
                fields: vec!["title".into()],
                min_doc_frequency: 1,
                filter: Some(Filter::Compare {
                    column: "category".into(),
                    operator: Comparison::Equal,
                    value: json!("audio"),
                }),
                ..Default::default()
            },
        )
        .unwrap();

    let id = result
        .columns
        .iter()
        .position(|column| column == "id")
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][id], json!(2));
    assert!(result.columns.contains(&"_score".into()));
}

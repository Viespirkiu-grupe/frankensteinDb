use super::*;

#[test]
fn phrase_prefix_matches_analyzed_multi_word_autocomplete() {
    let (_directory, mut database) = database();
    products(&mut database);

    let result = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::PhrasePrefix {
                column: "title".into(),
                phrase: "wireless head".into(),
                max_expansions: 50,
            }),
            vec![],
        ))
        .unwrap();

    assert_eq!(result.rows, vec![vec![json!(1)]]);
}

#[test]
fn disjunction_max_uses_best_field_and_configurable_tie_breaker() {
    let (_directory, mut database) = database();
    database
        .create_table_def(TableDef {
            name: "articles".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![
                test_column("id", ColumnType::Integer, true, false, None),
                test_column(
                    "title",
                    ColumnType::Text,
                    false,
                    false,
                    Some(Analyzer::Default),
                ),
                test_column(
                    "body",
                    ColumnType::Text,
                    false,
                    false,
                    Some(Analyzer::Default),
                ),
            ],
        })
        .unwrap();
    database
        .bulk_insert_json(
            "articles",
            &[
                vec![json!(1), json!("rust"), json!("rust")],
                vec![json!(2), json!("rust"), json!("database")],
            ],
        )
        .unwrap();

    let score = |database: &mut Database, tie_breaker| {
        database
            .read(read(
                "articles",
                vec![
                    Projection::Column {
                        column: "id".into(),
                        alias: None,
                    },
                    Projection::Score { alias: None },
                ],
                Some(Filter::DisjunctionMax {
                    fields: BTreeMap::from([("title".into(), 1.0), ("body".into(), 1.0)]),
                    query: "rust".into(),
                    tie_breaker,
                }),
                vec![],
            ))
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row[0] == json!(1))
            .unwrap()[1]
            .as_f64()
            .unwrap()
    };

    let best_field_only = score(&mut database, 0.0);
    let with_tie_breaker = score(&mut database, 0.5);
    assert!(
        with_tie_breaker > best_field_only,
        "scores: best={best_field_only}, tied={with_tie_breaker}"
    );
}

#[test]
fn structural_filters_do_not_change_relevance_scores() {
    let (_directory, mut database) = database();
    products(&mut database);
    let projection = vec![
        Projection::Column {
            column: "id".into(),
            alias: None,
        },
        Projection::Score { alias: None },
    ];
    let search = Filter::Search {
        fields: vec!["title".into()],
        query: "headphones".into(),
    };
    let baseline = database
        .read(read(
            "products",
            projection.clone(),
            Some(search.clone()),
            vec![],
        ))
        .unwrap();
    let filtered = database
        .read(read(
            "products",
            projection.clone(),
            Some(Filter::All {
                filters: vec![search, equal("active", json!(true))],
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(filtered.rows, baseline.rows);

    let filter_only = database
        .read(read(
            "products",
            projection,
            Some(equal("category", json!("audio"))),
            vec![],
        ))
        .unwrap();
    assert!(filter_only.rows.iter().all(|row| row[1] == json!(0.0)));
}

#[test]
fn score_explanation_targets_one_tantivy_document() {
    let (_directory, mut database) = database();
    products(&mut database);
    let request = read(
        "products",
        columns(&["id"]),
        Some(Filter::Search {
            fields: vec!["title".into()],
            query: "headphones".into(),
        }),
        vec![],
    );
    let explanation = database
        .explain_score(&request, &equal("id", json!(1)))
        .unwrap();
    assert!(explanation["score"].as_f64().unwrap() > 0.0);
    assert_eq!(explanation["score"], explanation["explanation"]["value"]);
    assert!(explanation["explanation"]["details"].is_array());

    let error = database
        .explain_score(&request, &equal("id", json!(3)))
        .unwrap_err();
    assert!(error.to_string().contains("does not match"));
}

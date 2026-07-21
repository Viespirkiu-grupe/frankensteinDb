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

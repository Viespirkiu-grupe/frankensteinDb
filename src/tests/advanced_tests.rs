use super::*;

#[test]
fn concurrent_search_service_reads_and_recursive_aggregations() {
    let (_directory, mut database) = database();
    products(&mut database);
    let search = database.search_service().unwrap();
    let left = search.clone();
    let right = search.clone();
    let first =
        std::thread::spawn(move || left.read(read("products", vec![], None, vec![])).unwrap());
    let second =
        std::thread::spawn(move || right.read(read("products", vec![], None, vec![])).unwrap());
    assert_eq!(first.join().unwrap().rows.len(), 3);
    assert_eq!(second.join().unwrap().rows.len(), 3);

    let aggregations = BTreeMap::from([(
        "categories".into(),
        Aggregation::Terms {
            column: "category".into(),
            size: 10,
            segment_size: None,
            min_doc_count: None,
            missing: None,
            order: None,
            aggregations: BTreeMap::from([(
                "average_price".into(),
                Aggregation::Metric {
                    function: Metric::Average,
                    column: Some("price".into()),
                    json_path: None,
                    percents: None,
                    missing: None,
                },
            )]),
        },
    )]);
    let result = search.aggregate("products", None, aggregations).unwrap();
    assert!(result["categories"]["buckets"].is_array());
}

#[test]
fn schema_changes_rebuild_rows_and_backup_restores_offline() {
    let (directory, mut database) = database();
    products(&mut database);
    database
        .change_table_schema(
            "products",
            vec![
                SchemaChange::RenameColumn {
                    from: "note".into(),
                    to: "description".into(),
                },
                SchemaChange::AddColumn {
                    column: test_column("stock", ColumnType::Integer, false, false, None),
                    default: json!(0),
                },
                SchemaChange::AlterColumn {
                    column: "price".into(),
                    definition: test_column(
                        "price",
                        ColumnType::Text,
                        false,
                        false,
                        Some(Analyzer::Raw),
                    ),
                },
            ],
        )
        .unwrap();
    let result = database
        .read(read("products", columns(&["id", "stock"]), None, vec![]))
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert!(result.rows.iter().all(|row| row[1] == json!(0)));

    let archive = directory.path().join("backup.tar.zst");
    database.backup_to(&archive).unwrap();
    drop(database);
    let restored = directory.path().join("restored");
    restore_backup(&restored, &archive, true).unwrap();
    let mut restored = Database::open(restored).unwrap();
    assert_eq!(
        restored
            .read(read("products", vec![], None, vec![]))
            .unwrap()
            .rows
            .len(),
        3
    );
}

#[test]
fn mutation_limit_rejects_the_exact_tantivy_match_set() {
    let (_directory, mut database) = database();
    products(&mut database);
    let mutation = Mutation::Delete {
        table: "products".into(),
        filter: Filter::Compare {
            column: "category".into(),
            operator: Comparison::Equal,
            value: json!("audio"),
        },
    };
    let error = database
        .mutate_typed_limited(mutation, Some(1))
        .unwrap_err();
    assert!(error.to_string().contains("exceeding max_rows 1"));
    assert_eq!(
        database
            .read(read("products", vec![], None, vec![]))
            .unwrap()
            .rows
            .len(),
        3
    );
}

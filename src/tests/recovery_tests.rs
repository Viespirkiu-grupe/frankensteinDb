use super::*;

#[test]
fn deferred_mutations_become_visible_only_after_flush() {
    let (_directory, mut database) = database();
    products(&mut database);
    database
        .mutate_typed_deferred(mutation_update(
            "title",
            json!("Deferred speakers"),
            equal("id", json!(1)),
        ))
        .unwrap();
    let search = || {
        read(
            "products",
            columns(&["id"]),
            Some(Filter::Search {
                fields: vec![],
                query: "deferred".into(),
            }),
            vec![],
        )
    };
    assert!(database.read(search()).unwrap().rows.is_empty());
    database.flush().unwrap();
    assert_eq!(database.read(search()).unwrap().rows, vec![vec![json!(1)]]);
}

#[test]
fn deferred_outbox_recovers_after_reopen() {
    let (directory, mut database) = database();
    products(&mut database);
    database
        .mutate_typed_deferred(mutation_update(
            "title",
            json!("Restart recovery"),
            equal("id", json!(1)),
        ))
        .unwrap();
    drop(database);
    let mut reopened = Database::open(directory.path()).unwrap();
    let result = reopened
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::Search {
                fields: vec![],
                query: "restart".into(),
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(1)]]);
}

#[test]
fn reindex_and_optimize_preserve_typed_reads() {
    let (_directory, mut database) = database();
    products(&mut database);
    database.reindex_table("products").unwrap();
    database.optimize_table("products").unwrap();
    assert_eq!(
        database
            .read(read("products", columns(&["id"]), None, vec![]))
            .unwrap()
            .rows
            .len(),
        3
    );
}

#[test]
fn optimize_retains_target_segments_and_reports_parallel_merges() {
    let directory = tempfile::tempdir().unwrap();
    let options = DatabaseOptions {
        min_merge_segments: 100,
        writer_threads: 1,
        ..DatabaseOptions::default()
    };
    let mut database = Database::open_with_options(directory.path(), options).unwrap();
    database
        .create_table_def(TableDef {
            name: "merge_groups".into(),
            aliases: vec![],
            document_store: Default::default(),
            columns: vec![test_column("id", ColumnType::Integer, true, false, None)],
        })
        .unwrap();
    for id in 0..9 {
        database
            .bulk_insert_json("merge_groups", &[vec![json!(id)]])
            .unwrap();
    }

    let result = database
        .optimize_table_with_options(
            "merge_groups",
            OptimizeOptions {
                target_segments: 3,
                merge_threads: 2,
            },
        )
        .unwrap();
    assert_eq!(result.segments_before, 9);
    assert_eq!(result.segments_after, 3);
    assert_eq!(result.merge_operations, 3);
    assert_eq!(result.merge_threads, 2);
    assert_eq!(
        database
            .read(read("merge_groups", columns(&["id"]), None, vec![]))
            .unwrap()
            .rows
            .len(),
        9
    );
}

#[test]
fn optimize_rejects_zero_target_segments() {
    let (_directory, mut database) = database();
    let error = database
        .optimize_table_with_options(
            "missing",
            OptimizeOptions {
                target_segments: 0,
                merge_threads: 1,
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("target_segments must be positive")
    );
}

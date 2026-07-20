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

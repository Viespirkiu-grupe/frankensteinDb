use std::collections::BTreeMap;

use super::*;

#[test]
fn typed_insert_update_and_delete_publish_incrementally() {
    let (directory, mut database) = database();
    products(&mut database);
    let sentinel = directory.path().join("indexes/products/keep");
    std::fs::write(&sentinel, b"not rebuilt").unwrap();

    database
        .mutate_typed(Mutation::Insert {
            table: "products".into(),
            row: BTreeMap::from([
                ("id".into(), json!(4)),
                ("title".into(), json!("Desk lamp")),
                ("category".into(), json!("home")),
                ("price".into(), json!(15.0)),
                ("active".into(), json!(true)),
                ("created_at".into(), json!("2026-07-20T11:00:00Z")),
                ("note".into(), Value::Null),
            ]),
        })
        .unwrap();
    database
        .mutate_typed(mutation_update(
            "title",
            json!("Reading lamp"),
            equal("id", json!(4)),
        ))
        .unwrap();
    database
        .mutate_typed(Mutation::Delete {
            table: "products".into(),
            filter: equal("id", json!(2)),
        })
        .unwrap();

    assert!(sentinel.exists());
    let found = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(Filter::Search {
                fields: vec!["title".into()],
                query: "reading".into(),
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(found.rows, vec![vec![json!(4)]]);
    assert!(
        database
            .read(read(
                "products",
                columns(&["id"]),
                Some(equal("id", json!(2))),
                vec![]
            ))
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn filter_based_update_and_delete_use_tantivy_candidates() {
    let (_directory, mut database) = database();
    products(&mut database);
    database
        .mutate_typed(mutation_update(
            "price",
            json!(55.0),
            Filter::All {
                filters: vec![
                    equal("category", json!("audio")),
                    equal("active", json!(true)),
                ],
            },
        ))
        .unwrap();
    let changed = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(equal("price", json!(55.0))),
            vec![Sort {
                column: "id".into(),
                json_path: None,
                json_type: None,
                descending: false,
            }],
        ))
        .unwrap();
    assert_eq!(changed.rows, vec![vec![json!(1)], vec![json!(2)]]);
    database
        .mutate_typed(Mutation::Delete {
            table: "products".into(),
            filter: Filter::Between {
                column: "price".into(),
                lower: json!(50),
                upper: json!(60),
            },
        })
        .unwrap();
    assert_eq!(
        database
            .read(read("products", columns(&["id"]), None, vec![]))
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn typed_batch_rolls_back_all_sqlite_writes_on_error() {
    let (_directory, mut database) = database();
    products(&mut database);
    let insert = |id, title| Mutation::Insert {
        table: "products".into(),
        row: BTreeMap::from([
            ("id".into(), json!(id)),
            ("title".into(), json!(title)),
            ("category".into(), json!("home")),
            ("price".into(), json!(10)),
            ("active".into(), json!(true)),
            ("created_at".into(), json!("2026-07-20T11:00:00Z")),
            ("note".into(), Value::Null),
        ]),
    };
    assert!(
        database
            .mutate_batch_typed(vec![insert(4, "Lamp"), insert(1, "Duplicate")])
            .is_err()
    );
    let result = database
        .read(read(
            "products",
            columns(&["id"]),
            Some(equal("id", json!(4))),
            vec![],
        ))
        .unwrap();
    assert!(result.rows.is_empty());
}

use std::collections::BTreeMap;

use super::*;

pub(super) fn database() -> (TempDir, Database) {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path()).unwrap();
    (directory, database)
}

pub(super) fn products(database: &mut Database) {
    database
        .create_table_def(products_table())
        .expect("create products table");
    database
        .bulk_insert_json(
            "products",
            &[
                vec![
                    json!(1),
                    json!("Wireless headphones"),
                    json!("audio"),
                    json!(79.5),
                    json!(true),
                    json!("2026-07-20T10:00:00Z"),
                    Value::Null,
                ],
                vec![
                    json!(2),
                    json!("Wired headphones"),
                    json!("audio"),
                    json!(29.0),
                    json!(true),
                    json!("2026-07-19T10:00:00Z"),
                    json!("sale"),
                ],
                vec![
                    json!(3),
                    json!("Wireless mouse"),
                    json!("computers"),
                    json!(45.0),
                    json!(false),
                    json!("2026-07-18T10:00:00Z"),
                    Value::Null,
                ],
            ],
        )
        .expect("insert products");
}

pub(super) fn products_table() -> TableDef {
    TableDef {
        name: "products".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            test_column("id", ColumnType::Integer, true, false, None),
            test_column(
                "title",
                ColumnType::Text,
                false,
                false,
                Some(Analyzer::Stem("english".into())),
            ),
            test_column(
                "category",
                ColumnType::Text,
                false,
                false,
                Some(Analyzer::Raw),
            ),
            test_column("price", ColumnType::Real, false, false, None),
            test_column("active", ColumnType::Boolean, false, false, None),
            test_column("created_at", ColumnType::DateTime, false, false, None),
            test_column(
                "note",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Default),
            ),
        ],
    }
}

pub(super) fn test_column(
    name: &str,
    data_type: ColumnType,
    primary_key: bool,
    nullable: bool,
    analyzer: Option<Analyzer>,
) -> ColumnDef {
    let compact_raw = analyzer == Some(Analyzer::Raw);
    ColumnDef {
        name: name.into(),
        data_type,
        primary_key,
        nullable,
        analyzer,
        compact_raw,
        index: Default::default(),
    }
}

pub(super) fn read(
    table: &str,
    projection: Vec<Projection>,
    filter: Option<Filter>,
    order_by: Vec<Sort>,
) -> ReadRequest {
    ReadRequest {
        table: table.into(),
        projection,
        filter,
        group_by: vec![],
        order_by,
        limit: 100,
        offset: 0,
        search_after: None,
        min_score: None,
    }
}

pub(super) fn columns(names: &[&str]) -> Vec<Projection> {
    names
        .iter()
        .map(|name| Projection::Column {
            column: (*name).into(),
            alias: None,
        })
        .collect()
}

pub(super) fn equal(column: &str, value: Value) -> Filter {
    Filter::Compare {
        column: column.into(),
        operator: Comparison::Equal,
        value,
    }
}

pub(super) fn mutation_update(column: &str, value: Value, filter: Filter) -> Mutation {
    Mutation::Update {
        table: "products".into(),
        values: BTreeMap::from([(column.into(), value)]),
        filter,
    }
}

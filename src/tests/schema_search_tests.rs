use super::*;

#[test]
fn typed_schema_validates_keys_analyzers_and_reserved_names() {
    let (_directory, mut database) = database();
    let mut table = products_table();
    table.columns[0].primary_key = false;
    assert!(database.create_table_def(table).is_err());

    let mut table = products_table();
    table.name = "invalid_analyzer".into();
    table.columns[1].analyzer = Some(Analyzer::Ngram {
        min: 0,
        max: 40,
        prefix_only: false,
    });
    assert!(database.create_table_def(table).is_err());

    let mut table = products_table();
    table.name = "__aq_private".into();
    assert!(database.create_table_def(table).is_err());
}

#[test]
fn canonical_schema_flattens_supplier_and_cpv_arrays() {
    let table = canonical_contract_table();
    assert_eq!(table.columns.len(), 20);
    assert_eq!(table.columns[13].name, "tiekejuKodai");
    assert_eq!(table.columns[17].name, "bvpzKodai");
    assert_eq!(table.columns[5].data_type, ColumnType::Timestamp);
}

#[test]
fn typed_datetime_and_timestamp_filters_validate_values() {
    let (_directory, mut database) = database();
    let table = TableDef {
        name: "events".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            test_column("id", ColumnType::Integer, true, false, None),
            test_column("day", ColumnType::Date, false, false, None),
            test_column("instant", ColumnType::Timestamp, false, false, None),
        ],
    };
    database.create_table_def(table).unwrap();
    database
        .bulk_insert_json(
            "events",
            &[vec![
                json!(1),
                json!("2026-07-20"),
                json!("2026-07-20T10:15:30.000"),
            ]],
        )
        .unwrap();
    let result = database
        .read(read(
            "events",
            columns(&["id"]),
            Some(Filter::Between {
                column: "day".into(),
                lower: json!("2026-01-01"),
                upper: json!("2026-12-31"),
            }),
            vec![],
        ))
        .unwrap();
    assert_eq!(result.rows, vec![vec![json!(1)]]);
}

#[test]
fn document_store_settings_are_applied_to_new_index_generations() {
    let (directory, mut database) = database();
    database.create_table_def(products_table()).unwrap();
    let default_index = Index::open_in_dir(directory.path().join("indexes/products")).unwrap();
    assert_eq!(
        default_index.settings().docstore_compression,
        Compressor::Lz4
    );
    assert_eq!(default_index.settings().docstore_blocksize, 16_384);
    assert!(default_index.settings().docstore_compress_dedicated_thread);

    let mut table = products_table();
    table.name = "products_zstd".into();
    table.document_store = DocumentStore {
        compression: DocumentCompression::Zstd,
        zstd_level: Some(1),
        block_size: 32_768,
        dedicated_thread: false,
    };
    database.create_table_def(table).unwrap();

    let index = Index::open_in_dir(directory.path().join("indexes/products_zstd")).unwrap();
    assert_eq!(index.settings().docstore_blocksize, 32_768);
    assert!(!index.settings().docstore_compress_dedicated_thread);
    assert_eq!(
        index.settings().docstore_compression,
        Compressor::Zstd(ZstdCompressor {
            compression_level: Some(1)
        })
    );
}

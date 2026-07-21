use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reuse_state_is_loaded_from_tantivy() {
        let directory = tempfile::tempdir().unwrap();
        let mut database = Database::open(directory.path()).unwrap();
        database
            .create_table_def(canonical_contract_table())
            .unwrap();
        let row = vec![
            json!(7),
            json!("Testas"),
            json!("2026-01-01"),
            Value::Null,
            Value::Null,
            json!("2026-01-01T10:00:00.000"),
            Value::Null,
            json!("123"),
            json!("Organizacija"),
            Value::Null,
            Value::Null,
            json!(100),
            Value::Null,
            json!(["456"]),
            json!(["Tiekėjas"]),
            json!("tipas"),
            Value::Null,
            json!([12345]),
            json!(false),
            json!(false),
        ];
        database
            .bulk_insert_json("sutartys", std::slice::from_ref(&row))
            .unwrap();

        let (count, id, loaded) = existing_benchmark_state(&mut database).unwrap();
        assert_eq!(count, 1);
        assert_eq!(id, 7);
        assert_eq!(loaded[0], row[0]);
        assert_eq!(loaded[13], row[13]);
        assert_eq!(loaded[17], row[17]);

        let mut capture = BenchmarkCapture::default();
        let measurements = run_benchmark_suite(
            &mut database,
            id,
            &loaded,
            1,
            &ProgressReporter::new(false),
            Some(&mut capture),
        )
        .unwrap();
        assert_eq!(measurements.len(), 37);
        let capture_path = directory.path().join("results.txt");
        capture.save(&capture_path).unwrap();
        let captured = std::fs::read_to_string(capture_path).unwrap();
        assert!(captured.starts_with("# FrankensteinDB benchmark query samples"));
        assert!(captured.contains("### Query\n\n```sql\nSELECT"));
        assert!(captured.contains("### Timing\n\n| Iterations"));
        assert!(captured.contains("### Sample result\n\n#### Row 1"));
        assert!(captured.contains("| Field"));
        assert!(captured.contains("Median (ms)"));
        assert!(!captured.trim_start().starts_with('{'));
        let calendar_composite = captured
            .split("calendar_composite_missing_order")
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap();
        assert!(calendar_composite.contains("CALENDAR_INTERVAL => MONTH"));
        assert!(calendar_composite.contains("#### `publication_months`"));
        assert!(calendar_composite.contains("doc_count"));
        assert!(!calendar_composite.contains("_No buckets returned._"));
    }

    #[test]
    fn benchmark_compression_defaults_to_none_and_accepts_zstd() {
        let defaults = Args::try_parse_from(["benchmark"]).unwrap();
        assert_eq!(defaults.compression, BenchmarkCompression::None);
        assert_eq!(defaults.docstore_block_size, 16_384);
        assert!(defaults.docstore_compression_thread);
        assert_eq!(defaults.save_results, None);

        let default_capture = Args::try_parse_from(["benchmark", "--save-results"]).unwrap();
        assert_eq!(default_capture.save_results, Some("results.txt".into()));
        let custom_capture =
            Args::try_parse_from(["benchmark", "--save-results", "target/custom-results.txt"])
                .unwrap();
        assert_eq!(
            custom_capture.save_results,
            Some("target/custom-results.txt".into())
        );

        let zstd = Args::try_parse_from([
            "benchmark",
            "--compression",
            "zstd",
            "--zstd-level",
            "5",
            "--docstore-block-size",
            "65536",
        ])
        .unwrap();
        validate_compression_args(&zstd).unwrap();
        assert_eq!(
            benchmark_document_store(&zstd),
            DocumentStore {
                compression: DocumentCompression::Zstd,
                zstd_level: Some(5),
                block_size: 65_536,
                dedicated_thread: true,
            }
        );
    }
}

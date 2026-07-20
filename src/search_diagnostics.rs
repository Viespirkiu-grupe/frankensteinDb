use super::*;
use crate::database_read::execute_typed_read;

impl SearchService {
    /// Executes a read and reports coarse compilation, matching, and materialization timings.
    pub fn profile(&self, request: ReadRequest) -> Result<Value> {
        let started = std::time::Instant::now();
        let handle = self.handle(&request.table)?;
        let lookup_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let compile_started = std::time::Instant::now();
        let fields = schema_fields(&handle.index.schema(), &handle.def)?;
        let searcher = handle.reader.searcher();
        validate_json_read_paths(&searcher, &handle.def, &request)?;
        let order = stable_typed_order(&handle.def, &request);
        let native_sort = typed_native_sort(&request, &handle.def, &order);
        let effective_filter =
            filter_after_cursor(&handle.def, &request, &order, native_sort.as_ref())?;
        let plan = compile_filter(
            &handle.index,
            &handle.def,
            &fields,
            effective_filter.as_ref(),
        )?;
        let compile_ms = compile_started.elapsed().as_secs_f64() * 1_000.0;
        let count_started = std::time::Instant::now();
        let matched = searcher.search(&*plan.query, &Count)?;
        let count_ms = count_started.elapsed().as_secs_f64() * 1_000.0;
        let execute_started = std::time::Instant::now();
        let result = execute_typed_read(&handle.def, &handle.index, &handle.reader, request)?;
        let execute_ms = execute_started.elapsed().as_secs_f64() * 1_000.0;
        Ok(json!({
            "engine": "tantivy",
            "matched_documents": matched,
            "returned_rows": result.rows.len(),
            "segments": searcher.segment_readers().len(),
            "timing_ms": {
                "catalog_lookup": lookup_ms,
                "query_compile": compile_ms,
                "count": count_ms,
                "execute_and_materialize": execute_ms,
                "total": started.elapsed().as_secs_f64() * 1_000.0
            }
        }))
    }

    /// Counts direct children below one hierarchical FACET root.
    pub fn facets(
        &self,
        table: &str,
        column_name: &str,
        root: &str,
        limit: usize,
        filter: Option<&Filter>,
    ) -> Result<Value> {
        ensure!(
            (1..=10_000).contains(&limit),
            "facet limit must be 1..=10000"
        );
        let handle = self.handle(table)?;
        let column = column(&handle.def, column_name)?;
        ensure!(
            matches!(column.data_type, ColumnType::Facet | ColumnType::FacetArray),
            "facet endpoint requires FACET or FACET[] column"
        );
        ensure!(root.starts_with('/'), "facet root must start with '/'");
        let fields = schema_fields(&handle.index.schema(), &handle.def)?;
        let searcher = handle.reader.searcher();
        validate_filter_only_json_paths(&searcher, &handle.def, filter)?;
        let query = compile_filter(&handle.index, &handle.def, &fields, filter)?.query;
        let mut collector = FacetCollector::for_field(&column.name);
        collector.add_facet(root);
        let counts = searcher.search(&*query, &collector)?;
        Ok(json!(
            counts
                .top_k(root, limit)
                .into_iter()
                .map(|(facet, count)| { json!({"path": facet.to_path_string(), "count": count}) })
                .collect::<Vec<_>>()
        ))
    }
}

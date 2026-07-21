use super::*;
use crate::aggregation_api::standard_aggregation_worker_count;
use crate::database_read::execute_typed_read;

impl SearchService {
    /// Executes a read and reports coarse compilation, matching, and materialization timings.
    pub fn profile(&self, request: ReadRequest) -> Result<Value> {
        self.profile_with_aggregations(request, BTreeMap::new())
    }

    /// Executes the same row and aggregation work as the HTTP query endpoint and reports timings.
    pub fn profile_with_aggregations(
        &self,
        request: ReadRequest,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Value> {
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
        let aggregation_started = std::time::Instant::now();
        let aggregation_count = aggregations.len();
        let standard_aggregation_count = aggregations
            .values()
            .filter(|aggregation| !matches!(aggregation, Aggregation::GeoTileGrid { .. }))
            .count();
        let aggregation_workers =
            standard_aggregation_worker_count(&searcher, standard_aggregation_count);
        if !aggregations.is_empty() {
            self.aggregate(&request.table, request.filter.as_ref(), aggregations)?;
        }
        let aggregation_ms = aggregation_started.elapsed().as_secs_f64() * 1_000.0;
        let execute_started = std::time::Instant::now();
        let returned_rows = if request.limit == 0 {
            0
        } else {
            execute_typed_read(&handle.def, &handle.index, &handle.reader, request)?
                .rows
                .len()
        };
        let execute_ms = execute_started.elapsed().as_secs_f64() * 1_000.0;
        Ok(json!({
            "engine": "tantivy",
            "matched_documents": matched,
            "returned_rows": returned_rows,
            "profiled_aggregations": aggregation_count,
            "aggregation_workers": aggregation_workers,
            "segments": searcher.segment_readers().len(),
            "timing_ms": {
                "catalog_lookup": lookup_ms,
                "query_compile": compile_ms,
                "count": count_ms,
                "aggregations": aggregation_ms,
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
        self.collect_facets(table, column_name, root, limit, filter, false)
    }

    /// Counts facet children after removing this facet column's structural predicates.
    pub fn facets_excluding_own_filter(
        &self,
        table: &str,
        column_name: &str,
        root: &str,
        limit: usize,
        filter: Option<&Filter>,
    ) -> Result<Value> {
        self.collect_facets(table, column_name, root, limit, filter, true)
    }

    fn collect_facets(
        &self,
        table: &str,
        column_name: &str,
        root: &str,
        limit: usize,
        filter: Option<&Filter>,
        exclude_own_filter: bool,
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
        let effective_filter = if exclude_own_filter {
            filter.and_then(|filter| filter_without_column(filter, column_name))
        } else {
            filter.cloned()
        };
        validate_filter_only_json_paths(&searcher, &handle.def, effective_filter.as_ref())?;
        let query = compile_filter(
            &handle.index,
            &handle.def,
            &fields,
            effective_filter.as_ref(),
        )?
        .query;
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

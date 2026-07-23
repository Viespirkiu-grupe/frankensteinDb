use super::*;
use crate::aggregation_api::{aggregation_range_worker_count, standard_aggregation_worker_count};
use crate::database_read::execute_typed_read;
use crate::segment_ranges::searcher_doc_ranges;

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
        let fields = Arc::clone(&handle.fields);
        let searcher = handle.reader.searcher();
        let json_cache = self.json_cache(&handle);
        validate_json_read_paths(&searcher, &handle.def, &request, Some(&json_cache))?;
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
        let aggregation_workers = standard_aggregation_worker_count(
            &searcher,
            standard_aggregation_count,
            &self.runtime.pool,
        );
        let aggregation_range_workers =
            aggregation_range_worker_count(&searcher, &self.runtime.pool);
        let sort_range_workers = searcher_doc_ranges(&searcher, self.runtime.worker_threads())
            .len()
            .min(self.runtime.worker_threads())
            .max(1);
        let aggregation_strategy = if aggregation_count == 0 {
            "none"
        } else if standard_aggregation_count == 0 {
            "geo"
        } else if aggregation_range_workers > 1 {
            if searcher.segment_readers().len() == 1 {
                "intra_segment_ranges"
            } else {
                "segment_ranges"
            }
        } else if aggregation_workers > 1 {
            "top_level_aggregations"
        } else {
            "tantivy_segments"
        };
        let parallel_block_sort =
            native_sort.as_ref().is_some_and(block_top_k_supported) && sort_range_workers > 1;
        if !aggregations.is_empty() {
            self.aggregate_uncached(&request.table, request.filter.as_ref(), aggregations)?;
        }
        let aggregation_ms = aggregation_started.elapsed().as_secs_f64() * 1_000.0;
        let execute_started = std::time::Instant::now();
        let returned_rows = if request.limit == 0 {
            0
        } else {
            execute_typed_read(
                &handle.def,
                &handle.index,
                &handle.reader,
                &handle.fields,
                request,
                &self.runtime.pool,
                Some(&json_cache),
            )?
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
            "aggregation_strategy": aggregation_strategy,
            "search_worker_threads": self.runtime.worker_threads(),
            "sort_workers": if parallel_block_sort { sort_range_workers } else { 1 },
            "sort_strategy": if parallel_block_sort {
                if searcher.segment_readers().len() == 1 {
                    "intra_segment_ranges"
                } else {
                    "segment_ranges"
                }
            } else if native_sort.as_ref().is_some_and(block_top_k_supported) {
                "block_top_k"
            } else {
                "tantivy"
            },
            "aggregation_cache_bypassed": true,
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
        let fields = Arc::clone(&handle.fields);
        let searcher = handle.reader.searcher();
        let effective_filter = if exclude_own_filter {
            filter.and_then(|filter| filter_without_column(filter, column_name))
        } else {
            filter.cloned()
        };
        let json_cache = self.json_cache(&handle);
        validate_filter_only_json_paths(
            &searcher,
            &handle.def,
            effective_filter.as_ref(),
            Some(&json_cache),
        )?;
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

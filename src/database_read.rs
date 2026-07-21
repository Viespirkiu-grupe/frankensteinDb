use super::*;

impl Database {
    /// Executes a typed read exclusively through Tantivy.
    pub fn read(&mut self, request: ReadRequest) -> Result<QueryResult> {
        self.recover()?;
        let def = self.table(&request.table)?;
        let index = self.index_handle(&def)?.index.clone();
        let reader = self.index_handle(&def)?.reader.clone();
        let pool = crate::search_runtime::system_search_pool()?;
        execute_typed_read(&def, &index, &reader, request, &pool)
    }

    /// Describes the Tantivy collector and typed read plan without executing the search.
    pub fn explain(&mut self, request: &ReadRequest) -> Result<QueryResult> {
        self.recover()?;
        let def = self.table(&request.table)?;
        let index = self.index_handle(&def)?.index.clone();
        let reader = self.index_handle(&def)?.reader.clone();
        explain_typed_read(&def, &index, &reader, request)
    }

    /// Explains the BM25 score of one Tantivy document selected by an identity filter.
    pub fn explain_score(&mut self, request: &ReadRequest, identity: &Filter) -> Result<Value> {
        self.recover()?;
        let def = self.table(&request.table)?;
        let index = self.index_handle(&def)?.index.clone();
        let reader = self.index_handle(&def)?.reader.clone();
        explain_typed_score(&def, &index, &reader, request, identity)
    }
}

pub(crate) fn explain_typed_score(
    def: &TableDef,
    index: &Index,
    reader: &IndexReader,
    request: &ReadRequest,
    identity: &Filter,
) -> Result<Value> {
    ensure!(
        request.group_by.is_empty(),
        "score explanation does not support aggregation"
    );
    let fields = schema_fields(&index.schema(), def)?;
    let searcher = reader.searcher();
    validate_json_read_paths(&searcher, def, request)?;
    validate_filter_only_json_paths(&searcher, def, Some(identity))?;
    let query = compile_filter(index, def, &fields, request.filter.as_ref())?.query;
    let identity_query = compile_filter(index, def, &fields, Some(identity))?.query;
    let documents = searcher.search(&*identity_query, &DocSetCollector)?;
    ensure!(
        documents.len() == 1,
        "identity filter must select exactly one document"
    );
    let address = *documents.iter().next().expect("one document");
    let explanation = query
        .explain(&searcher, address)
        .context("selected document does not match the query")?;
    Ok(json!({
        "score": explanation.value(),
        "document": {
            "segment_ord": address.segment_ord,
            "doc_id": address.doc_id
        },
        "explanation": explanation
    }))
}

pub(crate) fn execute_typed_read(
    def: &TableDef,
    index: &Index,
    reader: &IndexReader,
    request: ReadRequest,
    pool: &rayon::ThreadPool,
) -> Result<QueryResult> {
    let fields = schema_fields(&index.schema(), def)?;
    let searcher = reader.searcher();
    validate_json_read_paths(&searcher, def, &request)?;
    let order = stable_typed_order(def, &request);
    validate_typed_sort(def, &order)?;
    if typed_is_aggregation(&request) {
        ensure!(
            request.search_after.is_none(),
            "search_after is not supported for aggregations"
        );
        let plan = compile_filter(index, def, &fields, request.filter.as_ref())?;
        return execute_typed_aggregation(&searcher, &*plan.query, def, &request, &order);
    }
    let native_sort = typed_native_sort(&request, def, &order);
    let effective_filter = filter_after_cursor(def, &request, &order, native_sort.as_ref())?;
    let plan = compile_filter(index, def, &fields, effective_filter.as_ref())?;
    let cursor_mode = cursor_pagination_enabled(def, &request, &order, native_sort.as_ref());
    let full_scan = typed_requires_full_scan(&request, &order) && native_sort.is_none();
    let collection_limit = request
        .limit
        .saturating_add(usize::from(cursor_mode && request.limit > 0));
    let mut docs = collect_typed_docs(
        &searcher,
        &*plan.query,
        native_sort.as_ref(),
        full_scan,
        collection_limit,
        request.offset,
        pool,
    )?;
    if let Some(min_score) = request.min_score {
        ensure!(min_score.is_finite(), "min_score must be finite");
        docs.retain(|(score, _)| *score >= min_score);
    }
    let projected = required_typed_columns(def, &request, &order, full_scan || cursor_mode)?;
    let rows = load_typed_rows(&searcher, docs, &projected)?;
    let highlights =
        HighlightGenerators::create(&searcher, &index.schema(), def, &*plan.query, &request)?;
    project_typed_rows(
        def,
        &request,
        rows,
        &order,
        full_scan,
        cursor_mode,
        &highlights,
    )
}

pub(crate) fn explain_typed_read(
    def: &TableDef,
    index: &Index,
    reader: &IndexReader,
    request: &ReadRequest,
) -> Result<QueryResult> {
    validate_json_read_paths(&reader.searcher(), def, request)?;
    let fields = schema_fields(&index.schema(), def)?;
    let order = stable_typed_order(def, request);
    validate_typed_sort(def, &order)?;
    let aggregation = typed_is_aggregation(request);
    let native_sort = (!aggregation)
        .then(|| typed_native_sort(request, def, &order))
        .flatten();
    let effective_filter = filter_after_cursor(def, request, &order, native_sort.as_ref())?;
    compile_filter(index, def, &fields, effective_filter.as_ref())?;
    let collector = if aggregation {
        "aggregation"
    } else if native_sort.as_ref().is_some_and(block_top_k_supported) {
        "block_fast_field_top_docs"
    } else if native_sort.is_some() {
        "fast_field_top_docs"
    } else if typed_requires_full_scan(request, &order) {
        "full_scan_top_docs"
    } else {
        "score_top_docs"
    };
    Ok(QueryResult {
        columns: vec![
            "engine".into(),
            "table".into(),
            "collector".into(),
            "limit".into(),
            "offset".into(),
            "search_after".into(),
            "min_score".into(),
            "loads_sqlite_rows".into(),
        ],
        rows: vec![vec![
            json!("tantivy"),
            json!(request.table),
            json!(collector),
            json!(request.limit),
            json!(request.offset),
            json!(request.search_after),
            json!(request.min_score),
            json!(false),
        ]],
        message: "1 plan".into(),
        next_search_after: None,
    })
}

fn validate_typed_sort(def: &TableDef, order: &[OrderSpec]) -> Result<()> {
    for spec in order {
        if spec.key.eq_ignore_ascii_case("_score") {
            continue;
        }
        if spec.json_type.is_some() {
            ensure!(
                spec.geo_distance_from.is_none(),
                "geo distance sorting cannot target a JSON path"
            );
            continue;
        }
        if let Ok(column) = column(def, &spec.key) {
            if let Some(origin) = spec.geo_distance_from {
                origin.validate()?;
                ensure!(
                    matches!(
                        column.data_type,
                        ColumnType::GeoPoint | ColumnType::GeoPointArray
                    ),
                    "geo distance sorting requires a GEO_POINT or GEO_POINT[] column"
                );
                continue;
            }
            ensure!(
                !matches!(
                    column.data_type,
                    ColumnType::GeoPoint | ColumnType::GeoPointArray
                ),
                "GEO_POINT sorting requires geo_distance_from"
            );
            ensure!(
                !column.data_type.is_array(),
                "array columns cannot be sorted"
            );
        }
    }
    Ok(())
}

fn collect_typed_docs(
    searcher: &Searcher,
    query: &dyn Query,
    native_sort: Option<&NativeSort>,
    full_scan: bool,
    limit: usize,
    offset: usize,
    pool: &rayon::ThreadPool,
) -> Result<Vec<(f32, DocAddress)>> {
    if full_scan {
        let count = searcher.search(query, &Count)?;
        return if count == 0 {
            Ok(Vec::new())
        } else {
            Ok(searcher.search(query, &TopDocs::with_limit(count).order_by_score())?)
        };
    }
    if limit == 0 {
        return Ok(Vec::new());
    }
    if let Some(sort) = native_sort {
        return collect_native_sorted_docs(searcher, query, sort, limit, offset, pool);
    }
    Ok(searcher.search(
        query,
        &TopDocs::with_limit(limit)
            .and_offset(offset)
            .order_by_score(),
    )?)
}

pub(crate) fn load_typed_rows(
    searcher: &Searcher,
    docs: Vec<(f32, DocAddress)>,
    columns: &[&ColumnDef],
) -> Result<Vec<ResultRow>> {
    let mut fast_readers = HashMap::new();
    let mut rows = Vec::with_capacity(docs.len());
    for (score, address) in docs {
        if let std::collections::hash_map::Entry::Vacant(entry) =
            fast_readers.entry(address.segment_ord)
        {
            entry.insert(segment_fast_readers(
                searcher,
                address.segment_ord,
                columns,
            )?);
        }
        let mut values = HashMap::new();
        for reader in &fast_readers[&address.segment_ord] {
            values.insert(reader.name.clone(), reader.value(address.doc_id)?);
        }
        rows.push(ResultRow {
            values,
            score: score as f64,
        });
    }
    Ok(rows)
}

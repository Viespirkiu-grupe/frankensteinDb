use super::*;
use crate::aggregation_api::{
    aggregation_context, collect_intermediate, compile_aggregations, merge_intermediates,
};
use crate::database_read::{execute_typed_read, explain_typed_read, explain_typed_score};

impl SearchService {
    pub(crate) fn open(root: PathBuf, definitions: Vec<TableDef>) -> Result<Self> {
        let service = Self {
            root,
            tables: Arc::new(RwLock::new(HashMap::new())),
        };
        service.publish_catalog(definitions)?;
        Ok(service)
    }

    /// Returns the currently published table definitions without consulting SQLite.
    pub fn tables(&self) -> Result<Vec<TableDef>> {
        let tables = self
            .tables
            .read()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?;
        let mut definitions = tables
            .values()
            .map(|handle| handle.def.clone())
            .collect::<Vec<_>>();
        definitions.sort_by_key(|definition| definition.name.to_lowercase());
        Ok(definitions)
    }

    /// Returns one definition from the in-memory published catalog.
    pub fn table(&self, name: &str) -> Result<TableDef> {
        self.handle(name).map(|handle| handle.def)
    }

    /// Returns published table, segment, and live-document counts for observability.
    pub fn stats(&self) -> Result<(usize, usize, u64)> {
        let tables = self
            .tables
            .read()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?;
        let segments = tables
            .values()
            .map(|handle| handle.reader.searcher().segment_readers().len())
            .sum();
        let documents = tables
            .values()
            .map(|handle| handle.reader.searcher().num_docs())
            .sum();
        Ok((tables.len(), segments, documents))
    }

    /// Executes a typed read using only the published Tantivy snapshot.
    pub fn read(&self, request: ReadRequest) -> Result<QueryResult> {
        let handle = self.handle(&request.table)?;
        execute_typed_read(&handle.def, &handle.index, &handle.reader, request)
    }

    /// Explains a typed read using only in-memory metadata and Tantivy schema information.
    pub fn explain(&self, request: &ReadRequest) -> Result<QueryResult> {
        let handle = self.handle(&request.table)?;
        explain_typed_read(&handle.def, &handle.index, &handle.reader, request)
    }

    /// Explains the score of one identity-selected hit using Tantivy's explanation tree.
    pub fn explain_score(&self, request: &ReadRequest, identity: &Filter) -> Result<Value> {
        let handle = self.handle(&request.table)?;
        explain_typed_score(
            &handle.def,
            &handle.index,
            &handle.reader,
            request,
            identity,
        )
    }

    /// Executes a recursive aggregation tree through Tantivy.
    pub fn aggregate(
        &self,
        table: &str,
        filter: Option<&Filter>,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Value> {
        let handle = self.handle(table)?;
        let searcher = handle.reader.searcher();
        validate_filter_only_json_paths(&searcher, &handle.def, filter)?;
        validate_json_aggregation_paths(&searcher, &handle.def, &aggregations)?;
        let fields = schema_fields(&handle.index.schema(), &handle.def)?;
        let query = compile_filter(&handle.index, &handle.def, &fields, filter)?.query;
        let request = compile_aggregations(&handle.def, &aggregations)?;
        let collector =
            AggregationCollector::from_aggs(request, aggregation_context(&handle.index));
        Ok(serde_json::to_value(searcher.search(&*query, &collector)?)?)
    }

    /// Collects a mergeable, versioned binary Tantivy aggregation result for one shard.
    pub fn aggregate_intermediate(
        &self,
        table: &str,
        filter: Option<&Filter>,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Vec<u8>> {
        let handle = self.handle(table)?;
        let searcher = handle.reader.searcher();
        validate_filter_only_json_paths(&searcher, &handle.def, filter)?;
        validate_json_aggregation_paths(&searcher, &handle.def, &aggregations)?;
        let fields = schema_fields(&handle.index.schema(), &handle.def)?;
        let query = compile_filter(&handle.index, &handle.def, &fields, filter)?.query;
        let request = compile_aggregations(&handle.def, &aggregations)?;
        collect_intermediate(&searcher, &*query, &request, &handle.index)
    }

    /// Merges binary shard fruits and converts them into the final aggregation response.
    pub fn merge_aggregation_intermediates(
        &self,
        table: &str,
        aggregations: BTreeMap<String, Aggregation>,
        payloads: &[Vec<u8>],
    ) -> Result<Value> {
        let handle = self.handle(table)?;
        validate_json_aggregation_paths(&handle.reader.searcher(), &handle.def, &aggregations)?;
        merge_intermediates(compile_aggregations(&handle.def, &aggregations)?, payloads)
    }

    /// Publishes catalog changes and reloads existing readers after writer commits.
    pub fn publish_catalog(&self, definitions: Vec<TableDef>) -> Result<()> {
        let mut current = self
            .tables
            .write()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?;
        let mut next = HashMap::with_capacity(definitions.len());
        for def in definitions {
            if let Some(existing) = current.remove(&def.name)
                && serde_json::to_value(&existing.def)? == serde_json::to_value(&def)?
            {
                existing.reader.reload()?;
                next.insert(def.name.clone(), SearchHandle { def, ..existing });
                continue;
            }
            let index = Index::open_in_dir(self.root.join("indexes").join(&def.name))?;
            register_analyzers(&index, &def)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            next.insert(def.name.clone(), SearchHandle { def, index, reader });
        }
        *current = next;
        Ok(())
    }

    pub(crate) fn handle(&self, name: &str) -> Result<SearchHandle> {
        self.tables
            .read()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?
            .values()
            .find(|handle| {
                handle.def.name.eq_ignore_ascii_case(name)
                    || handle
                        .def
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(name))
            })
            .cloned()
            .ok_or_else(|| anyhow!("table not found: {name}"))
    }
}

impl Database {
    /// Creates a concurrent Tantivy-only read service for the current published catalog.
    pub fn search_service(&self) -> Result<SearchService> {
        SearchService::open(self.root.clone(), self.tables()?)
    }
}

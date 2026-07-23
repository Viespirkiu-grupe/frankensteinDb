use super::*;
use crate::aggregation_api::{
    collect_intermediate, collect_standard_aggregations, compile_aggregations, merge_intermediates,
};
use crate::database_read::{execute_typed_read, explain_typed_read, explain_typed_score};
use crate::search_runtime::AggregationCacheKey;

impl SearchService {
    pub(crate) fn open(root: PathBuf, definitions: Vec<TableDef>) -> Result<Self> {
        Self::open_with_options(root, definitions, SearchOptions::default())
    }

    pub(crate) fn open_with_options(
        root: PathBuf,
        definitions: Vec<TableDef>,
        options: SearchOptions,
    ) -> Result<Self> {
        let service = Self {
            root,
            tables: Arc::new(RwLock::new(SearchCatalog::default())),
            runtime: Arc::new(SearchRuntime::new(options)?),
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
            .canonical
            .values()
            .map(|handle| (*handle.def).clone())
            .collect::<Vec<_>>();
        definitions.sort_by_key(|definition| definition.name.to_lowercase());
        Ok(definitions)
    }

    /// Returns one definition from the in-memory published catalog.
    pub fn table(&self, name: &str) -> Result<TableDef> {
        self.handle(name).map(|handle| (*handle.def).clone())
    }

    /// Returns published table, segment, and live-document counts for observability.
    pub fn stats(&self) -> Result<(usize, usize, u64)> {
        let tables = self
            .tables
            .read()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?;
        let segments = tables
            .canonical
            .values()
            .map(|handle| handle.reader.searcher().segment_readers().len())
            .sum();
        let documents = tables
            .canonical
            .values()
            .map(|handle| handle.reader.searcher().num_docs())
            .sum();
        Ok((tables.canonical.len(), segments, documents))
    }

    /// Executes a typed read using only the published Tantivy snapshot.
    pub fn read(&self, request: ReadRequest) -> Result<QueryResult> {
        let handle = self.handle(&request.table)?;
        let json_cache = self.json_cache(&handle);
        execute_typed_read(
            &handle.def,
            &handle.index,
            &handle.reader,
            request,
            &self.runtime.pool,
            Some(&json_cache),
        )
    }

    /// Explains a typed read using only in-memory metadata and Tantivy schema information.
    pub fn explain(&self, request: &ReadRequest) -> Result<QueryResult> {
        let handle = self.handle(&request.table)?;
        let json_cache = self.json_cache(&handle);
        explain_typed_read(
            &handle.def,
            &handle.index,
            &handle.reader,
            request,
            Some(&json_cache),
        )
    }

    /// Explains the score of one identity-selected hit using Tantivy's explanation tree.
    pub fn explain_score(&self, request: &ReadRequest, identity: &Filter) -> Result<Value> {
        let handle = self.handle(&request.table)?;
        let json_cache = self.json_cache(&handle);
        explain_typed_score(
            &handle.def,
            &handle.index,
            &handle.reader,
            request,
            identity,
            Some(&json_cache),
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
        let cache_key =
            AggregationCacheKey::new(&handle.def.name, handle.generation, filter, &aggregations)?;
        if let Some(cached) = self.runtime.cached_aggregation(&cache_key) {
            return Ok(cached);
        }
        let value = self.aggregate_uncached_for_handle(&handle, filter, aggregations)?;
        self.runtime.cache_aggregation(cache_key, value.clone());
        Ok(value)
    }

    /// Executes aggregations without the result cache so diagnostics measure actual engine work.
    pub(crate) fn aggregate_uncached(
        &self,
        table: &str,
        filter: Option<&Filter>,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Value> {
        let handle = self.handle(table)?;
        self.aggregate_uncached_for_handle(&handle, filter, aggregations)
    }

    fn aggregate_uncached_for_handle(
        &self,
        handle: &SearchHandle,
        filter: Option<&Filter>,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Value> {
        let searcher = handle.reader.searcher();
        let json_cache = self.json_cache(handle);
        validate_filter_only_json_paths(&searcher, &handle.def, filter, Some(&json_cache))?;
        validate_json_aggregation_paths(&searcher, &handle.def, &aggregations, Some(&json_cache))?;
        let query = compile_filter(&handle.index, &handle.def, &handle.fields, filter)?.query;
        let mut standard = BTreeMap::new();
        let mut geo = Vec::new();
        for (name, aggregation) in aggregations {
            if matches!(aggregation, Aggregation::GeoTileGrid { .. }) {
                geo.push((name, aggregation));
            } else {
                standard.insert(name, aggregation);
            }
        }
        let mut result = serde_json::Map::new();
        if !standard.is_empty() {
            result.extend(collect_standard_aggregations(
                &searcher,
                &*query,
                &handle.def,
                &handle.index,
                standard,
                &self.runtime.pool,
            )?);
        }
        for (name, aggregation) in geo {
            result.insert(
                name,
                collect_geo_tile_grid(&searcher, &*query, &handle.def, &aggregation)?,
            );
        }
        Ok(Value::Object(result))
    }

    /// Collects a mergeable, versioned binary Tantivy aggregation result for one shard.
    pub fn aggregate_intermediate(
        &self,
        table: &str,
        filter: Option<&Filter>,
        aggregations: BTreeMap<String, Aggregation>,
    ) -> Result<Vec<u8>> {
        ensure!(
            !contains_geo_aggregation(&aggregations),
            "geo_tile_grid does not support distributed intermediate results"
        );
        let handle = self.handle(table)?;
        let searcher = handle.reader.searcher();
        let json_cache = self.json_cache(&handle);
        validate_filter_only_json_paths(&searcher, &handle.def, filter, Some(&json_cache))?;
        validate_json_aggregation_paths(&searcher, &handle.def, &aggregations, Some(&json_cache))?;
        let query = compile_filter(&handle.index, &handle.def, &handle.fields, filter)?.query;
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
        ensure!(
            !contains_geo_aggregation(&aggregations),
            "geo_tile_grid does not support distributed intermediate results"
        );
        let handle = self.handle(table)?;
        let json_cache = self.json_cache(&handle);
        validate_json_aggregation_paths(
            &handle.reader.searcher(),
            &handle.def,
            &aggregations,
            Some(&json_cache),
        )?;
        merge_intermediates(compile_aggregations(&handle.def, &aggregations)?, payloads)
    }

    /// Publishes catalog changes and reloads existing readers after writer commits.
    pub fn publish_catalog(&self, definitions: Vec<TableDef>) -> Result<()> {
        let mut current = self
            .tables
            .write()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?;
        let mut next = HashMap::with_capacity(definitions.len());
        let mut warmups = Vec::new();
        for def in definitions {
            let canonical_name = def.name.to_ascii_lowercase();
            if let Some(existing) = current.canonical.remove(&canonical_name)
                && serde_json::to_value(&*existing.def)? == serde_json::to_value(&def)?
            {
                let previous_segments = existing.reader.searcher().segment_readers().len();
                existing.reader.reload()?;
                let current_segments = existing.reader.searcher().segment_readers().len();
                let handle = Arc::new(SearchHandle {
                    def: Arc::new(def),
                    fields: Arc::clone(&existing.fields),
                    index: existing.index.clone(),
                    reader: existing.reader.clone(),
                    generation: existing.generation.saturating_add(1),
                });
                self.runtime.invalidate_table(&handle.def.name);
                if current_segments < previous_segments {
                    warmups.push(Arc::clone(&handle));
                }
                next.insert(canonical_name, handle);
                continue;
            }
            let index = Index::open_in_dir(self.root.join("indexes").join(&def.name))?;
            register_analyzers(&index, &def)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            let fields = Arc::new(schema_fields(&index.schema(), &def)?);
            let handle = Arc::new(SearchHandle {
                def: Arc::new(def),
                fields,
                index,
                reader,
                generation: 1,
            });
            self.runtime.invalidate_table(&handle.def.name);
            warmups.push(Arc::clone(&handle));
            next.insert(canonical_name, handle);
        }
        let mut lookup = HashMap::new();
        for handle in next.values() {
            lookup.insert(handle.def.name.to_ascii_lowercase(), Arc::clone(handle));
            for alias in &handle.def.aliases {
                lookup.insert(alias.to_ascii_lowercase(), Arc::clone(handle));
            }
        }
        *current = SearchCatalog {
            canonical: next,
            lookup,
        };
        drop(current);
        for handle in warmups {
            self.runtime.schedule_warmup(handle);
        }
        Ok(())
    }

    pub(crate) fn handle(&self, name: &str) -> Result<Arc<SearchHandle>> {
        self.tables
            .read()
            .map_err(|_| anyhow!("search catalog lock was poisoned"))?
            .lookup
            .get(&name.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| anyhow!("table not found: {name}"))
    }

    pub(crate) fn json_cache<'a>(&'a self, handle: &'a SearchHandle) -> JsonPathCacheContext<'a> {
        JsonPathCacheContext {
            runtime: &self.runtime,
            table: &handle.def.name,
            generation: handle.generation,
        }
    }
}

impl Database {
    /// Creates a concurrent Tantivy-only read service for the current published catalog.
    pub fn search_service(&self) -> Result<SearchService> {
        SearchService::open(self.root.clone(), self.tables()?)
    }

    /// Creates a concurrent read service with explicit worker, cache, and warmup settings.
    pub fn search_service_with_options(&self, options: SearchOptions) -> Result<SearchService> {
        SearchService::open_with_options(self.root.clone(), self.tables()?, options)
    }
}

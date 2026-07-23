use std::collections::{BTreeSet, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use rayon::prelude::*;
use sha2::{Digest, Sha256};

use super::*;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct AggregationCacheKey {
    table: String,
    generation: u64,
    request_hash: [u8; 32],
}

impl AggregationCacheKey {
    pub(crate) fn new(
        table: &str,
        generation: u64,
        filter: Option<&Filter>,
        aggregations: &BTreeMap<String, Aggregation>,
    ) -> Result<Self> {
        #[derive(serde::Serialize)]
        struct CacheRequest<'a> {
            filter: Option<&'a Filter>,
            aggregations: &'a BTreeMap<String, Aggregation>,
        }
        let mut hasher = Sha256::new();
        serde_json::to_writer(
            DigestWriter(&mut hasher),
            &CacheRequest {
                filter,
                aggregations,
            },
        )?;
        Ok(Self {
            table: table.to_ascii_lowercase(),
            generation,
            request_hash: hasher.finalize().into(),
        })
    }
}

struct DigestWriter<'a>(&'a mut Sha256);

impl std::io::Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) struct SearchRuntime {
    pub(crate) pool: Arc<rayon::ThreadPool>,
    warmup_pool: Arc<rayon::ThreadPool>,
    cache: Mutex<AggregationCache>,
    scheduled_warmups: Arc<Mutex<HashSet<(String, u64)>>>,
    warmup_fast_fields: bool,
    json_path_types: Mutex<HashMap<(String, u64, String), BTreeSet<DynamicColumnType>>>,
    wide_queries: WideQueryLimiter,
}

impl SearchRuntime {
    pub(crate) fn new(options: SearchOptions) -> Result<Self> {
        let pool = if options.worker_threads == 0 {
            system_search_pool()?
        } else {
            build_search_pool(options.worker_threads)?
        };
        Ok(Self {
            pool,
            warmup_pool: Arc::new(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .thread_name(|_| "frankensteindb-warmup".into())
                    .build()?,
            ),
            cache: Mutex::new(AggregationCache::new(options.aggregation_cache_entries)),
            scheduled_warmups: Arc::new(Mutex::new(HashSet::new())),
            warmup_fast_fields: options.warmup_fast_fields,
            json_path_types: Mutex::new(HashMap::new()),
            wide_queries: WideQueryLimiter::new(1),
        })
    }

    pub(crate) fn worker_threads(&self) -> usize {
        self.pool.current_num_threads()
    }

    pub(crate) fn acquire_wide_query(&self) -> Result<WideQueryPermit<'_>> {
        self.wide_queries.acquire()
    }

    pub(crate) fn cached_aggregation(&self, key: &AggregationCacheKey) -> Option<Value> {
        self.cache.lock().ok()?.get(key)
    }

    pub(crate) fn cache_aggregation(&self, key: AggregationCacheKey, value: Value) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, value);
        }
    }

    pub(crate) fn invalidate_table(&self, table: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove_table(table);
        }
        if let Ok(mut types) = self.json_path_types.lock() {
            types.retain(|(cached_table, _, _), _| !cached_table.eq_ignore_ascii_case(table));
        }
    }

    pub(crate) fn cached_json_path_types(
        &self,
        table: &str,
        generation: u64,
        path: &str,
        load: impl FnOnce() -> Result<BTreeSet<DynamicColumnType>>,
    ) -> Result<BTreeSet<DynamicColumnType>> {
        let key = (table.to_ascii_lowercase(), generation, path.to_owned());
        if let Ok(types) = self.json_path_types.lock()
            && let Some(observed) = types.get(&key)
        {
            return Ok(observed.clone());
        }
        let observed = load()?;
        if let Ok(mut types) = self.json_path_types.lock() {
            types.insert(key, observed.clone());
        }
        Ok(observed)
    }

    pub(crate) fn schedule_warmup(&self, handle: Arc<SearchHandle>) {
        if !self.warmup_fast_fields {
            return;
        }
        let key = (handle.def.name.to_ascii_lowercase(), handle.generation);
        let should_schedule = self
            .scheduled_warmups
            .lock()
            .map(|mut warmups| warmups.insert(key.clone()))
            .unwrap_or(false);
        if should_schedule {
            let scheduled_warmups = Arc::clone(&self.scheduled_warmups);
            let pool = Arc::clone(&self.pool);
            self.warmup_pool.spawn(move || {
                let _ = warm_fast_fields(&handle, &pool);
                if let Ok(mut warmups) = scheduled_warmups.lock() {
                    warmups.remove(&key);
                }
            });
        }
    }
}

struct WideQueryLimiter {
    active: Mutex<usize>,
    available: Condvar,
    limit: usize,
}

impl WideQueryLimiter {
    fn new(limit: usize) -> Self {
        Self {
            active: Mutex::new(0),
            available: Condvar::new(),
            limit: limit.max(1),
        }
    }

    fn acquire(&self) -> Result<WideQueryPermit<'_>> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow!("wide-query limiter lock was poisoned"))?;
        while *active >= self.limit {
            active = self
                .available
                .wait(active)
                .map_err(|_| anyhow!("wide-query limiter lock was poisoned"))?;
        }
        *active += 1;
        Ok(WideQueryPermit { limiter: self })
    }
}

pub(crate) struct WideQueryPermit<'a> {
    limiter: &'a WideQueryLimiter,
}

impl Drop for WideQueryPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.limiter.active.lock() {
            *active = active.saturating_sub(1);
            self.limiter.available.notify_one();
        }
    }
}

static SYSTEM_SEARCH_POOL: OnceLock<Arc<rayon::ThreadPool>> = OnceLock::new();

pub(crate) fn system_search_pool() -> Result<Arc<rayon::ThreadPool>> {
    if let Some(pool) = SYSTEM_SEARCH_POOL.get() {
        return Ok(Arc::clone(pool));
    }
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let candidate = build_search_pool(workers)?;
    let _ = SYSTEM_SEARCH_POOL.set(Arc::clone(&candidate));
    Ok(SYSTEM_SEARCH_POOL.get().cloned().unwrap_or(candidate))
}

fn build_search_pool(worker_threads: usize) -> Result<Arc<rayon::ThreadPool>> {
    ensure!(worker_threads > 0, "search worker_threads must be positive");
    Ok(Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_threads)
            .thread_name(|index| format!("frankensteindb-search-{index}"))
            .build()?,
    ))
}

struct AggregationCache {
    values: Option<lru::LruCache<AggregationCacheKey, Value>>,
}

impl AggregationCache {
    fn new(capacity: usize) -> Self {
        Self {
            values: NonZeroUsize::new(capacity).map(lru::LruCache::new),
        }
    }

    fn get(&mut self, key: &AggregationCacheKey) -> Option<Value> {
        self.values.as_mut()?.get(key).cloned()
    }

    fn insert(&mut self, key: AggregationCacheKey, value: Value) {
        if let Some(values) = &mut self.values {
            values.put(key, value);
        }
    }

    fn remove_table(&mut self, table: &str) {
        if let Some(values) = &mut self.values {
            let keys = values
                .iter()
                .filter(|(key, _)| key.table.eq_ignore_ascii_case(table))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in keys {
                values.pop(&key);
            }
        }
    }
}

fn warm_fast_fields(handle: &SearchHandle, pool: &rayon::ThreadPool) -> Result<()> {
    let searcher = handle.reader.searcher();
    let fields = handle
        .def
        .columns
        .iter()
        .filter(|column| {
            matches!(
                column.data_type,
                ColumnType::Integer
                    | ColumnType::Unsigned
                    | ColumnType::Real
                    | ColumnType::Boolean
                    | ColumnType::Date
                    | ColumnType::DateTime
                    | ColumnType::Timestamp
                    | ColumnType::Text
                    | ColumnType::TextArray
                    | ColumnType::Facet
                    | ColumnType::FacetArray
            )
        })
        .map(|column| (aggregation_field(column), column.data_type))
        .collect::<Vec<_>>();
    let work = searcher
        .segment_readers()
        .iter()
        .flat_map(|segment| {
            fields
                .iter()
                .map(move |(field, data_type)| (segment, field, *data_type))
        })
        .collect::<Vec<_>>();
    pool.install(|| {
        work.into_par_iter()
            .try_for_each(|(segment, field, data_type)| {
                let max_doc = segment.max_doc();
                if matches!(
                    data_type,
                    ColumnType::Text
                        | ColumnType::TextArray
                        | ColumnType::Facet
                        | ColumnType::FacetArray
                ) {
                    if let Some(column) = segment.fast_fields().str(field)? {
                        warm_string_column(&column, max_doc)?;
                    }
                } else if let Some((column, _)) = segment.fast_fields().u64_lenient(field)? {
                    warm_column(&column, max_doc);
                }
                Ok(())
            })
    })
}

fn warm_column(column: &Column<u64>, max_doc: DocId) {
    const BLOCK_SIZE: usize = 4_096;
    let mut docs = Vec::with_capacity(BLOCK_SIZE);
    let mut values = vec![None; BLOCK_SIZE];
    for start in (0..max_doc).step_by(BLOCK_SIZE) {
        let end = max_doc.min(start.saturating_add(BLOCK_SIZE as u32));
        docs.clear();
        docs.extend(start..end);
        column.first_vals(&docs, &mut values[..docs.len()]);
    }
}

fn warm_string_column(column: &StrColumn, max_doc: DocId) -> Result<()> {
    warm_column(column.ords(), max_doc);
    let mut value = String::new();
    for ordinal in 0..column.num_terms() as u64 {
        value.clear();
        column.ord_to_str(ordinal, &mut value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregation_cache_is_lru_and_invalidates_by_table() {
        let mut cache = AggregationCache::new(2);
        let key = |table: &str, byte| AggregationCacheKey {
            table: table.into(),
            generation: 1,
            request_hash: [byte; 32],
        };
        cache.insert(key("a", 1), json!(1));
        cache.insert(key("a", 2), json!(2));
        assert_eq!(cache.get(&key("a", 1)), Some(json!(1)));
        cache.insert(key("b", 3), json!(3));
        assert_eq!(cache.get(&key("a", 2)), None);
        cache.remove_table("a");
        assert_eq!(cache.get(&key("a", 1)), None);
        assert_eq!(cache.get(&key("b", 3)), Some(json!(3)));
    }

    #[test]
    fn wide_query_limiter_releases_waiters_when_permit_drops() {
        let limiter = Arc::new(WideQueryLimiter::new(1));
        let first = limiter.acquire().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiting = Arc::clone(&limiter);
        let thread = std::thread::spawn(move || {
            let _permit = waiting.acquire().unwrap();
            sender.send(()).unwrap();
        });

        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        drop(first);
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        thread.join().unwrap();
    }
}

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

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
        let request = json!({"filter": filter, "aggregations": aggregations});
        Ok(Self {
            table: table.to_ascii_lowercase(),
            generation,
            request_hash: Sha256::digest(serde_json::to_vec(&request)?).into(),
        })
    }
}

pub(crate) struct SearchRuntime {
    pub(crate) pool: rayon::ThreadPool,
    cache: Mutex<AggregationCache>,
    scheduled_warmups: Arc<Mutex<HashSet<(String, u64)>>>,
    warmup_fast_fields: bool,
}

impl SearchRuntime {
    pub(crate) fn new(options: SearchOptions) -> Result<Self> {
        let worker_threads = if options.worker_threads == 0 {
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
        } else {
            options.worker_threads
        };
        ensure!(worker_threads > 0, "search worker_threads must be positive");
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(worker_threads)
            .thread_name(|index| format!("frankensteindb-search-{index}"))
            .build()?;
        Ok(Self {
            pool,
            cache: Mutex::new(AggregationCache::new(options.aggregation_cache_entries)),
            scheduled_warmups: Arc::new(Mutex::new(HashSet::new())),
            warmup_fast_fields: options.warmup_fast_fields,
        })
    }

    pub(crate) fn worker_threads(&self) -> usize {
        self.pool.current_num_threads()
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
    }

    pub(crate) fn schedule_warmup(&self, handle: SearchHandle) {
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
            self.pool.spawn(move || {
                let _ = warm_fast_fields(&handle);
                if let Ok(mut warmups) = scheduled_warmups.lock() {
                    warmups.remove(&key);
                }
            });
        }
    }
}

struct AggregationCache {
    capacity: usize,
    values: HashMap<AggregationCacheKey, Value>,
    recency: VecDeque<AggregationCacheKey>,
}

impl AggregationCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            values: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &AggregationCacheKey) -> Option<Value> {
        let value = self.values.get(key)?.clone();
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: AggregationCacheKey, value: Value) {
        if self.capacity == 0 {
            return;
        }
        self.values.insert(key.clone(), value);
        self.touch(&key);
        while self.values.len() > self.capacity {
            if let Some(oldest) = self.recency.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, key: &AggregationCacheKey) {
        self.recency.retain(|candidate| candidate != key);
        self.recency.push_back(key.clone());
    }

    fn remove_table(&mut self, table: &str) {
        self.values
            .retain(|key, _| !key.table.eq_ignore_ascii_case(table));
        self.recency
            .retain(|key| !key.table.eq_ignore_ascii_case(table));
    }
}

fn warm_fast_fields(handle: &SearchHandle) -> Result<()> {
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
    for segment in searcher.segment_readers() {
        let max_doc = segment.max_doc();
        for (field, data_type) in &fields {
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
        }
    }
    Ok(())
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
    for doc in 0..max_doc {
        for ordinal in column.term_ords(doc) {
            std::hint::black_box(ordinal);
        }
    }
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
}

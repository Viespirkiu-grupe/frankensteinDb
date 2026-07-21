use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::{DateTime as ChronoDateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params, types::ValueRef};
use serde_json::{Number, Value, json};
use std::ops::Bound;
use tantivy::aggregation::{
    AggContextParams, AggregationCollector, AggregationLimitsGuard, agg_req::Aggregations,
};
use tantivy::collector::sort_key::{Comparator, ComparatorEnum};
use tantivy::collector::{
    Count, DocSetCollector, FacetCollector, SegmentSortKeyComputer, SortKeyComputer, TopDocs,
};
use tantivy::columnar::{
    BytesColumn, Column, ColumnType as DynamicColumnType, MonotonicallyMappableToU64, StrColumn,
};
use tantivy::merge_policy::LogMergePolicy;
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, DisjunctionMaxQuery, ExistsQuery,
    FuzzyTermQuery, MoreLikeThisQuery, Occur, PhrasePrefixQuery, Query, QueryParser, RangeQuery,
    RegexPhraseQuery, RegexQuery, TermQuery, TermSetQuery,
};
use tantivy::schema::{Facet, IndexRecordOption, IntoIpv6Addr, OwnedValue, TantivyDocument, Term};
use tantivy::snippet::SnippetGenerator;
use tantivy::store::{Compressor, ZstdCompressor};
use tantivy::{
    DateTime, DocAddress, DocId, Index, IndexReader, IndexSettings, IndexWriter, Order,
    ReloadPolicy, Score, Searcher, SegmentReader,
};

const CATALOG_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS __aq_tables (
    name TEXT PRIMARY KEY,
    schema_json TEXT NOT NULL,
    dirty INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS __aq_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name TEXT NOT NULL,
    operations_json TEXT NOT NULL
);
"#;
mod canonical;
pub use canonical::{canonical_contract_row, canonical_contract_table};

mod filter;
pub use filter::{Comparison, Filter};

mod geo;
pub use geo::{
    GEO_MAX_ZOOM, GeoBounds, GeoDistanceMode, GeoPoint, GeoTileCountMode, haversine_distance_meters,
};
use geo::{
    collect_geo_tile_grid, compile_geo_bounds, compile_geo_distance_comparison, compile_geo_radius,
    contains_geo_aggregation, decode_points, distance_for_points, geo_coordinate_field,
};

mod aggregation_model;
pub use aggregation_model::{
    Aggregation, AggregationRange, BucketOrder, CalendarInterval, CompositeSource, HistogramBounds,
    Metric, MissingOrder,
};

mod model;
pub use model::{
    Aggregate, Analyzer, ColumnDef, ColumnType, DocumentCompression, DocumentStore, IndexProfile,
    JsonPath, JsonPathType, Mutation, Projection, QueryResult, ReadRequest, SchemaChange, Sort,
    TableDef, TextIndexRecord,
};
use model::{RowOperation, RowValue};

/// Embedded SQLite/Tantivy database. Writes are durable in SQLite; supported reads use Tantivy.
pub struct Database {
    root: PathBuf,
    conn: Connection,
    indexes: HashMap<String, IndexHandle>,
    deferred_outbox_ids: HashSet<i64>,
    staged_outbox_ids: HashSet<i64>,
    options: DatabaseOptions,
}

/// Thread-safe Tantivy-only view of the published database state.
///
/// Readers never access SQLite. Catalog and reader changes are published explicitly by the
/// writer after a successful commit, so independent queries can execute concurrently.
#[derive(Clone)]
pub struct SearchService {
    root: PathBuf,
    tables: Arc<RwLock<HashMap<String, SearchHandle>>>,
}

#[derive(Clone)]
struct SearchHandle {
    def: TableDef,
    index: Index,
    reader: IndexReader,
}

#[derive(Debug, Clone)]
/// Runtime controls for Tantivy indexing and segment merging.
pub struct DatabaseOptions {
    /// Total memory shared by Tantivy indexing workers for each actively written table.
    pub writer_memory_bytes: usize,
    /// Number of Tantivy indexing workers per actively written table.
    pub writer_threads: usize,
    /// Merge a segment once this fraction of its documents has been deleted.
    pub deleted_docs_merge_ratio: f32,
    /// Smallest number of similarly sized segments considered for a merge.
    pub min_merge_segments: usize,
    /// Number of SQLite WAL pages between automatic checkpoints.
    pub sqlite_wal_autocheckpoint_pages: u32,
    /// SQLite WAL synchronization policy. `Full` fsyncs each commit; `Normal` syncs checkpoints.
    pub sqlite_synchronous: SqliteSynchronous,
}

/// SQLite durability/performance policy used while writing the WAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSynchronous {
    /// Sync the WAL on every commit, including protection against sudden power loss.
    Full,
    /// Preserve consistency but allow recent commits to disappear after sudden power loss.
    Normal,
    /// Skip SQLite sync calls. Intended only for disposable imports.
    Off,
}

impl SqliteSynchronous {
    fn pragma_value(self) -> &'static str {
        match self {
            Self::Full => "FULL",
            Self::Normal => "NORMAL",
            Self::Off => "OFF",
        }
    }
}

impl Default for DatabaseOptions {
    fn default() -> Self {
        Self {
            writer_memory_bytes: 50_000_000,
            writer_threads: 2,
            deleted_docs_merge_ratio: 0.2,
            min_merge_segments: 4,
            sqlite_wal_autocheckpoint_pages: 16_384,
            sqlite_synchronous: SqliteSynchronous::Full,
        }
    }
}

struct IndexHandle {
    index: Index,
    reader: IndexReader,
    writer: Option<IndexWriter>,
}

mod database_admin;
mod database_api;
mod database_backup;
mod database_index;
mod database_mutation;
mod database_read;
pub use database_backup::restore_backup;
mod aggregation_api;
mod database_schema;
mod document_store;
mod search_diagnostics;
mod similar;
pub use similar::MoreLikeThisOptions;
mod search_service;

struct QueryPlan {
    query: Box<dyn Query>,
}

fn new_index_writer(index: &Index, options: &DatabaseOptions) -> Result<IndexWriter> {
    let writer =
        index.writer_with_num_threads(options.writer_threads, options.writer_memory_bytes)?;
    let mut merge_policy = LogMergePolicy::default();
    merge_policy.set_min_num_segments(options.min_merge_segments);
    merge_policy.set_del_docs_ratio_before_merge(options.deleted_docs_merge_ratio);
    writer.set_merge_policy(Box::new(merge_policy));
    Ok(writer)
}

mod mutation;
use mutation::*;

mod query;
use query::*;

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

mod sql_schema;
use sql_schema::*;

mod tantivy_schema;
use tantivy_schema::*;
mod synonym_filter;

impl QueryResult {
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self {
            columns: vec![],
            rows: vec![],
            message: message.into(),
            next_search_after: None,
        }
    }
}
mod tantivy_array;

#[cfg(test)]
mod tests;

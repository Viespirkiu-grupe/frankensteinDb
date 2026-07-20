use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::Filter;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
/// Logical column types supported by FrankensteinDB and their Tantivy projections.
pub enum ColumnType {
    Integer,
    Unsigned,
    Real,
    Text,
    Boolean,
    Date,
    DateTime,
    Timestamp,
    TextArray,
    IntegerArray,
    UnsignedArray,
    RealArray,
    BooleanArray,
    DateArray,
    DateTimeArray,
    TimestampArray,
    BlobArray,
    IpArray,
    JsonArray,
    FacetArray,
    Blob,
    Ip,
    Json,
    Facet,
}

impl ColumnType {
    /// Returns true when one logical column may contain multiple scalar values.
    pub const fn is_array(&self) -> bool {
        matches!(
            self,
            Self::TextArray
                | Self::IntegerArray
                | Self::UnsignedArray
                | Self::RealArray
                | Self::BooleanArray
                | Self::DateArray
                | Self::DateTimeArray
                | Self::TimestampArray
                | Self::BlobArray
                | Self::IpArray
                | Self::JsonArray
                | Self::FacetArray
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Amount of term information retained for a searchable text field.
pub enum TextIndexRecord {
    /// Terms only; optimized for filters and exact matching.
    Basic,
    /// Terms and frequencies; supports BM25 without phrase queries.
    Frequencies,
    /// Terms, frequencies, and positions; supports BM25 and phrase queries.
    #[default]
    Positions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Tantivy indexing controls for one logical column.
pub struct IndexProfile {
    /// Whether the value is queryable. Values remain fast fields so reads stay Tantivy-only.
    #[serde(default = "default_true")]
    pub indexed: bool,
    /// Text index detail. Ignored by non-text columns.
    #[serde(default)]
    pub record: TextIndexRecord,
    /// Also retain the value in Tantivy's compressed document store.
    #[serde(default)]
    pub stored: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Compression algorithm used by Tantivy's stored-document blocks.
pub enum DocumentCompression {
    /// Fast compression and decompression; Tantivy's default when LZ4 support is enabled.
    #[default]
    Lz4,
    /// Better compression density at a configurable CPU cost.
    Zstd,
    /// Store blocks without compression.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Per-table Tantivy document-store settings applied when an index generation is created.
pub struct DocumentStore {
    /// Stored-document compression algorithm.
    #[serde(default)]
    pub compression: DocumentCompression,
    /// Zstd level. `None` uses Zstd's default level 3 and is ignored by other algorithms.
    #[serde(default)]
    pub zstd_level: Option<i32>,
    /// Uncompressed bytes collected into one document-store block.
    #[serde(default = "default_document_store_block_size")]
    pub block_size: usize,
    /// Compress document-store blocks on Tantivy's dedicated compression thread.
    #[serde(default = "default_true")]
    pub dedicated_thread: bool,
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self {
            compression: DocumentCompression::Lz4,
            zstd_level: None,
            block_size: default_document_store_block_size(),
            dedicated_thread: true,
        }
    }
}

const fn default_document_store_block_size() -> usize {
    16_384
}

impl Default for IndexProfile {
    fn default() -> Self {
        Self {
            indexed: true,
            record: TextIndexRecord::Positions,
            stored: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Tokenization strategy used by searchable text and text-array columns.
pub enum Analyzer {
    Default,
    Raw,
    Whitespace,
    Stem(String),
    Ngram {
        min: usize,
        max: usize,
        prefix_only: bool,
    },
    /// Configurable Latin-language analyzer pipeline.
    Custom {
        #[serde(default)]
        stem: Option<String>,
        #[serde(default)]
        stop_words: Vec<String>,
        /// One-token synonym expansions emitted at the same token position.
        #[serde(default)]
        synonyms: BTreeMap<String, Vec<String>>,
        #[serde(default)]
        ascii_folding: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Catalog metadata for one typed column.
pub struct ColumnDef {
    pub name: String,
    pub data_type: ColumnType,
    pub primary_key: bool,
    pub nullable: bool,
    pub analyzer: Option<Analyzer>,
    #[serde(default)]
    pub compact_raw: bool,
    #[serde(default)]
    pub index: IndexProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Durable catalog definition shared by SQLite and Tantivy.
pub struct TableDef {
    pub name: String,
    /// Alternate API names resolving to the same published Tantivy generation.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Tantivy stored-document compression. Existing schemas default to LZ4 with 16 KiB blocks.
    #[serde(default)]
    pub document_store: DocumentStore,
    pub columns: Vec<ColumnDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum RowValue {
    Null,
    Integer(i64),
    Unsigned(u64),
    Real(f64),
    Text(String),
    TextArray(Vec<String>),
    IntegerArray(Vec<i64>),
    UnsignedArray(Vec<u64>),
    RealArray(Vec<f64>),
    BooleanArray(Vec<bool>),
    BlobArray(Vec<Vec<u8>>),
    JsonArray(Vec<Value>),
    Blob(Vec<u8>),
    Json(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum RowOperation {
    Delete {
        key: RowValue,
    },
    Upsert {
        row: Vec<RowValue>,
    },
    /// Re-read the current row from SQLite during crash recovery.
    Refresh {
        key: RowValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Tabular result returned by typed database operations.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub message: String,
    /// Sort values for the next keyset page, when another page is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_search_after: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// A typed Tantivy read request. An empty projection returns every table column.
pub struct ReadRequest {
    pub table: String,
    #[serde(default)]
    pub projection: Vec<Projection>,
    #[serde(default)]
    pub filter: Option<Filter>,
    #[serde(default)]
    pub group_by: Vec<String>,
    #[serde(default)]
    pub order_by: Vec<Sort>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    /// Sort values returned by the previous page. Requires an explicit native `order_by`.
    #[serde(default)]
    pub search_after: Option<Vec<Value>>,
    /// Reject hits below this BM25 score. Requires a scored query.
    #[serde(default)]
    pub min_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// A selected column, relevance score, or aggregate metric.
pub enum Projection {
    Column {
        column: String,
        alias: Option<String>,
    },
    Score {
        alias: Option<String>,
    },
    Highlight {
        column: String,
        alias: Option<String>,
        #[serde(default = "default_fragment_size")]
        fragment_size: usize,
    },
    Aggregate {
        function: Aggregate,
        column: Option<String>,
        alias: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Aggregate functions executed by Tantivy collectors.
pub enum Aggregate {
    Count,
    Sum,
    Average,
    Min,
    Max,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// One stable sort key. `column` may also name a projection alias or `_score`.
pub struct Sort {
    pub column: String,
    /// Optional dotted path when `column` names a JSON or JSON[] column.
    #[serde(default)]
    pub json_path: Option<String>,
    /// Required dynamic type for `json_path` sorting.
    #[serde(default)]
    pub json_type: Option<JsonPathType>,
    #[serde(default)]
    pub descending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// A dynamically typed scalar path inside a JSON or JSON[] column.
pub struct JsonPath {
    pub column: String,
    pub path: String,
    pub data_type: JsonPathType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
/// Scalar types emitted by Tantivy's dynamic JSON fast fields.
pub enum JsonPathType {
    String,
    I64,
    U64,
    F64,
    Bool,
    /// RFC 3339 string auto-detected and indexed by Tantivy as a date-time.
    DateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// A typed durable write operation.
pub enum Mutation {
    Insert {
        table: String,
        row: BTreeMap<String, Value>,
    },
    Update {
        table: String,
        values: BTreeMap<String, Value>,
        filter: Filter,
    },
    Delete {
        table: String,
        filter: Filter,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// One atomic schema change executed through a shadow-table rebuild.
pub enum SchemaChange {
    AddColumn {
        column: ColumnDef,
        #[serde(default)]
        default: Value,
    },
    DropColumn {
        column: String,
    },
    RenameColumn {
        from: String,
        to: String,
    },
    AlterColumn {
        column: String,
        definition: ColumnDef,
    },
    /// Rebuild the Tantivy generation with new stored-document settings.
    AlterDocumentStore {
        document_store: DocumentStore,
    },
}

const fn default_limit() -> usize {
    100
}

const fn default_true() -> bool {
    true
}

const fn default_fragment_size() -> usize {
    160
}

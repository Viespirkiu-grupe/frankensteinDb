use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Filter, JsonPath, Sort};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Recursive bucket or metric aggregation compiled to Tantivy collectors.
pub enum Aggregation {
    /// Buckets values from one typed table column.
    Terms {
        column: String,
        #[serde(default = "default_bucket_size")]
        size: usize,
        #[serde(default)]
        segment_size: Option<usize>,
        #[serde(default)]
        min_doc_count: Option<u64>,
        #[serde(default)]
        missing: Option<Value>,
        #[serde(default)]
        order: Option<BucketOrder>,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Fixed-width numeric buckets from one typed table column.
    Histogram {
        column: String,
        interval: f64,
        #[serde(default)]
        offset: Option<f64>,
        #[serde(default)]
        min_doc_count: u64,
        #[serde(default)]
        hard_bounds: Option<HistogramBounds>,
        #[serde(default)]
        extended_bounds: Option<HistogramBounds>,
        #[serde(default)]
        keyed: bool,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Fixed-duration buckets from a typed date or timestamp column.
    DateHistogram {
        column: String,
        fixed_interval: String,
        #[serde(default)]
        offset: Option<String>,
        #[serde(default)]
        min_doc_count: u64,
        #[serde(default)]
        hard_bounds: Option<HistogramBounds>,
        #[serde(default)]
        extended_bounds: Option<HistogramBounds>,
        #[serde(default)]
        keyed: bool,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Explicit numeric or date ranges from one typed table column.
    Range {
        column: String,
        ranges: Vec<AggregationRange>,
        #[serde(default)]
        keyed: bool,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Terms buckets from a dynamically typed JSON path.
    JsonTerms {
        target: JsonPath,
        #[serde(default = "default_bucket_size")]
        size: usize,
        #[serde(default)]
        missing: Option<Value>,
        #[serde(default)]
        order: Option<BucketOrder>,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Fixed-width numeric buckets from a JSON path.
    JsonHistogram {
        target: JsonPath,
        interval: f64,
        #[serde(default)]
        min_doc_count: u64,
        #[serde(default)]
        hard_bounds: Option<HistogramBounds>,
        #[serde(default)]
        extended_bounds: Option<HistogramBounds>,
        #[serde(default)]
        keyed: bool,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Explicit numeric ranges from a JSON path.
    JsonRange {
        target: JsonPath,
        ranges: Vec<AggregationRange>,
        #[serde(default)]
        keyed: bool,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// A filter bucket with optional child aggregations.
    Filter {
        filter: Filter,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// Paginable multi-source buckets.
    Composite {
        sources: Vec<CompositeSource>,
        #[serde(default = "default_bucket_size")]
        size: usize,
        #[serde(default)]
        after: BTreeMap<String, Value>,
        #[serde(default)]
        aggregations: BTreeMap<String, Aggregation>,
    },
    /// A scalar metric over either a column or a JSON path.
    Metric {
        function: Metric,
        column: Option<String>,
        #[serde(default)]
        json_path: Option<JsonPath>,
        #[serde(default)]
        percents: Option<Vec<f64>>,
        #[serde(default)]
        missing: Option<Value>,
    },
    /// Representative documents selected inside a bucket.
    TopHits {
        size: usize,
        #[serde(default)]
        sort: Vec<Sort>,
        #[serde(default)]
        columns: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Number of non-missing values.
    Count,
    /// Sum of numeric values.
    Sum,
    /// Arithmetic mean of numeric values.
    Average,
    /// Lowest value.
    Min,
    /// Highest value.
    Max,
    /// Approximate distinct-value count.
    Cardinality,
    /// Requested percentile values.
    Percentiles,
    /// Count, min, max, average, and sum.
    Stats,
    /// Stats plus variance and standard deviation.
    ExtendedStats,
}

/// One optionally named range bucket with inclusive lower and exclusive upper bounds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregationRange {
    /// Required when the parent range aggregation is keyed.
    pub key: Option<String>,
    /// Optional inclusive lower bound.
    pub from: Option<Value>,
    /// Optional exclusive upper bound.
    pub to: Option<Value>,
}

/// Lower and upper limits for histogram bucket generation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistogramBounds {
    /// Lowest allowed or extended bucket boundary.
    pub min: Value,
    /// Highest allowed or extended bucket boundary.
    pub max: Value,
}

/// Ordering applied to terms buckets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BucketOrder {
    /// `_count`, `_key`, or a single-value metric sub-aggregation name.
    pub target: String,
    /// Sort high-to-low when true.
    #[serde(default)]
    pub descending: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissingOrder {
    /// Use Tantivy's default placement for the selected direction.
    #[default]
    Default,
    /// Place the missing bucket before concrete values.
    First,
    /// Place the missing bucket after concrete values.
    Last,
}

/// Calendar units supported by composite date-histogram sources.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalendarInterval {
    /// Calendar year.
    Year,
    /// Calendar month.
    Month,
    /// ISO calendar week.
    Week,
}

/// One named source of composite bucket keys.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompositeSource {
    /// Terms from a typed table column.
    Terms {
        name: String,
        column: String,
        #[serde(default)]
        descending: bool,
        #[serde(default)]
        missing_bucket: bool,
        #[serde(default)]
        missing_order: MissingOrder,
    },
    /// Fixed-width numeric buckets from a typed table column.
    Histogram {
        name: String,
        column: String,
        interval: f64,
        #[serde(default)]
        descending: bool,
        #[serde(default)]
        missing_bucket: bool,
        #[serde(default)]
        missing_order: MissingOrder,
    },
    /// Fixed-duration or calendar date buckets from a typed table column.
    DateHistogram {
        name: String,
        column: String,
        #[serde(default)]
        fixed_interval: Option<String>,
        #[serde(default)]
        calendar_interval: Option<CalendarInterval>,
        #[serde(default)]
        descending: bool,
        #[serde(default)]
        missing_bucket: bool,
        #[serde(default)]
        missing_order: MissingOrder,
    },
    /// Terms from a dynamically typed JSON path.
    JsonTerms {
        name: String,
        target: JsonPath,
        #[serde(default)]
        descending: bool,
        #[serde(default)]
        missing_bucket: bool,
        #[serde(default)]
        missing_order: MissingOrder,
    },
}

const fn default_bucket_size() -> usize {
    10
}

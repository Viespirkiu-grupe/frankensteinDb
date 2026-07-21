use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::JsonPathType;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
/// Composable typed filter compiled directly into a Tantivy query.
pub enum Filter {
    Compare {
        column: String,
        operator: Comparison,
        value: Value,
    },
    Between {
        column: String,
        lower: Value,
        upper: Value,
    },
    In {
        column: String,
        values: Vec<Value>,
    },
    IsNull {
        column: String,
        negated: bool,
    },
    Search {
        fields: Vec<String>,
        query: String,
    },
    /// BM25 full-text search with explicit per-field boosts.
    SearchBoosted {
        fields: BTreeMap<String, f32>,
        query: String,
        #[serde(default)]
        conjunction_by_default: bool,
    },
    /// Typo-tolerant single-term search.
    Fuzzy {
        column: String,
        value: String,
        #[serde(default = "default_fuzzy_distance")]
        distance: u8,
        #[serde(default)]
        transposition_cost_one: bool,
    },
    /// Prefix/autocomplete search against a text field.
    Prefix {
        column: String,
        value: String,
    },
    /// Matches an analyzed phrase whose final token is an autocomplete prefix.
    PhrasePrefix {
        column: String,
        phrase: String,
        #[serde(default = "default_phrase_prefix_max_expansions")]
        max_expansions: u32,
    },
    /// Uses the best matching field score plus a fraction of the remaining field scores.
    DisjunctionMax {
        fields: BTreeMap<String, f32>,
        query: String,
        #[serde(default)]
        tie_breaker: f32,
    },
    /// Matches any indexed token accepted by a regular expression.
    Regex {
        column: String,
        pattern: String,
    },
    /// Matches a sequence of per-token regular expressions in one positional text field.
    RegexPhrase {
        column: String,
        patterns: Vec<String>,
        #[serde(default)]
        slop: u32,
        #[serde(default = "default_regex_max_expansions")]
        max_expansions: u32,
    },
    /// Searches one dotted path inside a JSON column.
    JsonSearch {
        column: String,
        path: String,
        query: String,
    },
    /// Typed equality or range comparison on one dynamic JSON fast-field path.
    JsonCompare {
        column: String,
        path: String,
        data_type: JsonPathType,
        operator: Comparison,
        value: Value,
    },
    /// Inclusive typed range on one dynamic JSON fast-field path.
    JsonBetween {
        column: String,
        path: String,
        data_type: JsonPathType,
        lower: Value,
        upper: Value,
    },
    /// Tests whether any scalar dynamic type exists at one JSON path.
    JsonExists {
        column: String,
        path: String,
        #[serde(default)]
        data_type: Option<JsonPathType>,
        #[serde(default)]
        negated: bool,
    },
    /// Matches documents having any point within a great-circle radius.
    GeoDistance {
        column: String,
        center: crate::GeoPoint,
        radius_meters: f64,
    },
    /// Matches documents having any point inside a WGS84 rectangle.
    GeoBoundingBox {
        column: String,
        bounds: crate::GeoBounds,
    },
    /// Compares a reduced document distance. Primarily used by stable geo cursors.
    GeoDistanceCompare {
        column: String,
        center: crate::GeoPoint,
        #[serde(default)]
        mode: crate::GeoDistanceMode,
        operator: Comparison,
        distance_meters: f64,
    },
    All {
        filters: Vec<Filter>,
    },
    Any {
        filters: Vec<Filter>,
    },
    Not {
        filter: Box<Filter>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// Scalar comparison operators supported by typed filters.
pub enum Comparison {
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

const fn default_fuzzy_distance() -> u8 {
    1
}

const fn default_regex_max_expansions() -> u32 {
    4_096
}

const fn default_phrase_prefix_max_expansions() -> u32 {
    50
}

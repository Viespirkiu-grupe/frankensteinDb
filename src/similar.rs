use serde::{Deserialize, Serialize};

use super::*;
use crate::database_read::{execute_typed_read, load_typed_rows};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
/// Controls lexical document-similarity search through Tantivy MoreLikeThis.
pub struct MoreLikeThisOptions {
    /// Indexed fields used to derive representative terms. Empty selects indexed text fields.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Optional typed filter applied without affecting similarity score.
    #[serde(default)]
    pub filter: Option<Filter>,
    #[serde(default = "default_min_doc_frequency")]
    pub min_doc_frequency: u64,
    pub max_doc_frequency: Option<u64>,
    #[serde(default = "default_min_term_frequency")]
    pub min_term_frequency: usize,
    #[serde(default = "default_max_query_terms")]
    pub max_query_terms: usize,
    pub min_word_length: Option<usize>,
    pub max_word_length: Option<usize>,
    #[serde(default = "default_boost_factor")]
    pub boost_factor: f32,
    #[serde(default)]
    pub stop_words: Vec<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub min_score: Option<f32>,
}

impl Default for MoreLikeThisOptions {
    fn default() -> Self {
        Self {
            fields: vec![],
            filter: None,
            min_doc_frequency: default_min_doc_frequency(),
            max_doc_frequency: None,
            min_term_frequency: default_min_term_frequency(),
            max_query_terms: default_max_query_terms(),
            min_word_length: None,
            max_word_length: None,
            boost_factor: default_boost_factor(),
            stop_words: vec![],
            limit: default_limit(),
            min_score: None,
        }
    }
}

impl SearchService {
    /// Finds documents lexically similar to one primary-key row without reading SQLite.
    pub fn more_like_this(
        &self,
        table: &str,
        key: Value,
        options: MoreLikeThisOptions,
    ) -> Result<QueryResult> {
        validate_options(&options)?;
        let handle = self.handle(table)?;
        let fields = similarity_columns(&handle.def, &options.fields)?;
        let seed = load_seed(&handle, key.clone(), &fields)?;
        let document_fields = similarity_values(&handle.index, &fields, &seed)?;
        let query = similarity_query(&handle, key, &options, document_fields)?;
        let searcher = handle.reader.searcher();
        let mut docs = searcher.search(
            &*query,
            &TopDocs::with_limit(options.limit).order_by_score(),
        )?;
        if let Some(min_score) = options.min_score {
            docs.retain(|(score, _)| *score >= min_score);
        }
        let columns = handle.def.columns.iter().collect::<Vec<_>>();
        let rows = load_typed_rows(&searcher, docs, &columns)?;
        let mut names = handle
            .def
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        names.push("_score".into());
        let values = rows
            .into_iter()
            .map(|row| {
                let mut values = handle
                    .def
                    .columns
                    .iter()
                    .map(|column| row.values.get(&column.name).cloned().unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                values.push(json!(row.score));
                values
            })
            .collect::<Vec<_>>();
        Ok(QueryResult {
            columns: names,
            message: format!("{} similar row(s)", values.len()),
            rows: values,
            next_search_after: None,
        })
    }
}

fn validate_options(options: &MoreLikeThisOptions) -> Result<()> {
    ensure!(
        (1..=1_000).contains(&options.limit),
        "similar limit must be 1..=1000"
    );
    ensure!(
        (1..=1_024).contains(&options.max_query_terms),
        "max_query_terms must be 1..=1024"
    );
    ensure!(
        options.boost_factor.is_finite() && options.boost_factor > 0.0,
        "boost_factor must be positive"
    );
    ensure!(
        options.min_score.is_none_or(f32::is_finite),
        "min_score must be finite"
    );
    Ok(())
}

fn similarity_columns<'a>(def: &'a TableDef, requested: &[String]) -> Result<Vec<&'a ColumnDef>> {
    let columns = if requested.is_empty() {
        def.columns
            .iter()
            .filter(|column| {
                column.index.indexed
                    && matches!(column.data_type, ColumnType::Text | ColumnType::TextArray)
            })
            .collect::<Vec<_>>()
    } else {
        requested
            .iter()
            .map(|name| column(def, name))
            .collect::<Result<Vec<_>>>()?
    };
    ensure!(
        !columns.is_empty(),
        "MoreLikeThis requires at least one field"
    );
    for column in &columns {
        ensure!(
            column.index.indexed,
            "similarity field is not indexed: {}",
            column.name
        );
        ensure!(
            supports_similarity(column.data_type),
            "unsupported MoreLikeThis field: {}",
            column.name
        );
    }
    Ok(columns)
}

fn supports_similarity(data_type: ColumnType) -> bool {
    matches!(
        data_type,
        ColumnType::Text
            | ColumnType::TextArray
            | ColumnType::Integer
            | ColumnType::IntegerArray
            | ColumnType::Unsigned
            | ColumnType::UnsignedArray
            | ColumnType::Real
            | ColumnType::RealArray
            | ColumnType::Date
            | ColumnType::DateArray
            | ColumnType::DateTime
            | ColumnType::DateTimeArray
            | ColumnType::Timestamp
            | ColumnType::TimestampArray
            | ColumnType::Facet
            | ColumnType::FacetArray
    )
}

fn load_seed(handle: &SearchHandle, key: Value, fields: &[&ColumnDef]) -> Result<Vec<Value>> {
    let primary_key = &handle.def.columns[primary_key_index(&handle.def)];
    let request = ReadRequest {
        table: handle.def.name.clone(),
        projection: fields
            .iter()
            .map(|column| Projection::Column {
                column: column.name.clone(),
                alias: None,
            })
            .collect(),
        filter: Some(Filter::Compare {
            column: primary_key.name.clone(),
            operator: Comparison::Equal,
            value: key,
        }),
        group_by: vec![],
        order_by: vec![],
        limit: 1,
        offset: 0,
        search_after: None,
        min_score: None,
    };
    execute_typed_read(&handle.def, &handle.index, &handle.reader, request)?
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("seed row not found"))
}

fn similarity_values(
    index: &Index,
    fields: &[&ColumnDef],
    seed: &[Value],
) -> Result<Vec<(tantivy::schema::Field, Vec<OwnedValue>)>> {
    fields
        .iter()
        .zip(seed)
        .map(|(column, value)| {
            let field = index.schema().get_field(&column.name)?;
            Ok((field, owned_values(column.data_type, value)?))
        })
        .collect()
}

fn owned_values(data_type: ColumnType, value: &Value) -> Result<Vec<OwnedValue>> {
    let values = if data_type.is_array() {
        value.as_array().context("expected seed array")?.clone()
    } else {
        vec![value.clone()]
    };
    values
        .into_iter()
        .map(|value| owned_scalar(data_type, value))
        .collect()
}

fn owned_scalar(data_type: ColumnType, value: Value) -> Result<OwnedValue> {
    Ok(match data_type {
        ColumnType::Text | ColumnType::TextArray => {
            OwnedValue::Str(value.as_str().context("expected text")?.into())
        }
        ColumnType::Integer | ColumnType::IntegerArray => {
            OwnedValue::I64(value.as_i64().context("expected integer")?)
        }
        ColumnType::Unsigned | ColumnType::UnsignedArray => {
            OwnedValue::U64(value.as_u64().context("expected unsigned")?)
        }
        ColumnType::Real | ColumnType::RealArray => {
            OwnedValue::F64(value.as_f64().context("expected number")?)
        }
        ColumnType::Date | ColumnType::DateArray => {
            let date = chrono::NaiveDate::parse_from_str(
                value.as_str().context("expected date")?,
                "%Y-%m-%d",
            )?;
            OwnedValue::Date(DateTime::from_timestamp_micros(
                date.and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_micros(),
            ))
        }
        ColumnType::DateTime | ColumnType::DateTimeArray => {
            OwnedValue::Date(DateTime::from_timestamp_micros(
                ChronoDateTime::parse_from_rfc3339(value.as_str().context("expected datetime")?)?
                    .timestamp_micros(),
            ))
        }
        ColumnType::Timestamp | ColumnType::TimestampArray => OwnedValue::Date(
            timestamp_to_tantivy(value.as_str().context("expected timestamp")?)?,
        ),
        ColumnType::Facet | ColumnType::FacetArray => {
            OwnedValue::Facet(Facet::from(value.as_str().context("expected facet")?))
        }
        _ => bail!("unsupported MoreLikeThis value"),
    })
}

fn similarity_query(
    handle: &SearchHandle,
    key: Value,
    options: &MoreLikeThisOptions,
    values: Vec<(tantivy::schema::Field, Vec<OwnedValue>)>,
) -> Result<Box<dyn Query>> {
    let mut builder = MoreLikeThisQuery::builder()
        .with_min_doc_frequency(options.min_doc_frequency)
        .with_min_term_frequency(options.min_term_frequency)
        .with_max_query_terms(options.max_query_terms)
        .with_boost_factor(options.boost_factor)
        .with_stop_words(options.stop_words.clone());
    if let Some(value) = options.max_doc_frequency {
        builder = builder.with_max_doc_frequency(value);
    }
    if let Some(value) = options.min_word_length {
        builder = builder.with_min_word_length(value);
    }
    if let Some(value) = options.max_word_length {
        builder = builder.with_max_word_length(value);
    }
    let mut clauses: Vec<(Occur, Box<dyn Query>)> =
        vec![(Occur::Must, Box::new(builder.with_document_fields(values)))];
    let fields = schema_fields(&handle.index.schema(), &handle.def)?;
    if let Some(filter) = &options.filter {
        let filter = compile_filter(&handle.index, &handle.def, &fields, Some(filter))?.query;
        clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(filter, 0.0))));
    }
    let primary_key = &handle.def.columns[primary_key_index(&handle.def)];
    clauses.push((
        Occur::MustNot,
        term_query(term_for_value(&fields, primary_key, &key)?),
    ));
    Ok(Box::new(BooleanQuery::new(clauses)))
}

const fn default_min_doc_frequency() -> u64 {
    2
}
const fn default_min_term_frequency() -> usize {
    1
}
const fn default_max_query_terms() -> usize {
    25
}
const fn default_boost_factor() -> f32 {
    1.0
}
const fn default_limit() -> usize {
    20
}

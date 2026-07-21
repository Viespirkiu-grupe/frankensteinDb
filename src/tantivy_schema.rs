use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use chrono::DateTime as ChronoDateTime;
use rusqlite::types::ValueRef;
use tantivy::schema::{
    BytesOptions, DateOptions, DateTimePrecision, FAST, FacetOptions, Field, IndexRecordOption,
    IntoIpv6Addr, IpAddrOptions, JsonObjectOptions, NumericOptions, OwnedValue, STRING, Schema,
    TantivyDocument, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, NgramTokenizer, RemoveLongFilter, SimpleTokenizer, Stemmer,
    StopWordFilter, TextAnalyzer,
};
use tantivy::{DateTime, Index};

use super::timestamp_to_tantivy;
use crate::TextIndexRecord;
use crate::geo::{geo_coordinate_field, geo_latitude_field, geo_longitude_field, index_geo_points};
use crate::model::RowValue;
use crate::sql_schema::{analyzer_name, language};
use crate::synonym_filter::SynonymFilter;
use crate::tantivy_array::{add_row_array, add_sql_array};
use crate::{Analyzer, ColumnDef, ColumnType, TableDef};

#[derive(Clone)]
pub(super) struct IndexFields {
    pub(super) values: HashMap<String, Field>,
    pub(super) raw: HashMap<String, Field>,
    pub(super) arrays: HashMap<String, Field>,
    pub(super) geo_coordinates: HashMap<String, Field>,
    pub(super) geo_latitudes: HashMap<String, Field>,
    pub(super) geo_longitudes: HashMap<String, Field>,
}

pub(super) fn build_tantivy_schema(def: &TableDef) -> Schema {
    let mut builder = Schema::builder();
    for column in &def.columns {
        match column.data_type {
            ColumnType::Integer | ColumnType::IntegerArray => {
                builder.add_i64_field(&column.name, numeric_options(column));
            }
            ColumnType::Unsigned | ColumnType::UnsignedArray => {
                builder.add_u64_field(&column.name, numeric_options(column));
            }
            ColumnType::Real | ColumnType::RealArray => {
                builder.add_f64_field(&column.name, numeric_options(column));
            }
            ColumnType::Boolean | ColumnType::BooleanArray => {
                builder.add_bool_field(&column.name, numeric_options(column));
            }
            ColumnType::Date
            | ColumnType::DateTime
            | ColumnType::Timestamp
            | ColumnType::DateArray
            | ColumnType::DateTimeArray
            | ColumnType::TimestampArray => {
                let mut options = DateOptions::default()
                    .set_fast()
                    .set_precision(DateTimePrecision::Milliseconds);
                if column.index.indexed {
                    options = options.set_indexed();
                }
                if column.index.stored {
                    options = options.set_stored();
                }
                builder.add_date_field(&column.name, options);
            }
            ColumnType::Blob | ColumnType::BlobArray => {
                builder.add_bytes_field(&column.name, bytes_options(column));
            }
            ColumnType::Text | ColumnType::TextArray => {
                if column.compact_raw || !column.index.indexed {
                    let mut options = TextOptions::default().set_fast(Some("raw"));
                    if column.index.indexed {
                        options = options.set_indexing_options(
                            TextFieldIndexing::default()
                                .set_tokenizer("raw")
                                .set_index_option(IndexRecordOption::Basic),
                        );
                    }
                    if column.index.stored {
                        options = options.set_stored();
                    }
                    builder.add_text_field(&column.name, options);
                } else {
                    let indexing = TextFieldIndexing::default()
                        .set_tokenizer(&analyzer_name(column))
                        .set_index_option(text_record(column.index.record));
                    let mut options = TextOptions::default().set_indexing_options(indexing);
                    if column.index.stored {
                        options = options.set_stored();
                    }
                    builder.add_text_field(&column.name, options);
                    builder.add_text_field(&format!("__aq_raw_{}", column.name), STRING | FAST);
                }
            }
            ColumnType::Ip | ColumnType::IpArray => {
                let mut options = IpAddrOptions::default().set_fast();
                if column.index.indexed {
                    options = options.set_indexed();
                }
                if column.index.stored {
                    options = options.set_stored();
                }
                builder.add_ip_addr_field(&column.name, options);
            }
            ColumnType::Json | ColumnType::JsonArray => {
                let indexing = TextFieldIndexing::default()
                    .set_tokenizer("default")
                    .set_index_option(text_record(column.index.record));
                let mut options = JsonObjectOptions::default().set_fast(None);
                if column.index.indexed {
                    options = options.set_indexing_options(indexing);
                }
                if column.index.stored {
                    options = options.set_stored();
                }
                builder.add_json_field(&column.name, options);
                builder.add_bytes_field(&format!("__aq_raw_{}", column.name), FAST);
            }
            ColumnType::Facet | ColumnType::FacetArray => {
                let options = if column.index.stored {
                    FacetOptions::default().set_stored()
                } else {
                    FacetOptions::default()
                };
                builder.add_facet_field(&column.name, options);
                builder.add_bytes_field(&format!("__aq_raw_{}", column.name), FAST);
            }
            ColumnType::GeoPoint | ColumnType::GeoPointArray => {
                builder.add_u64_field(&column.name, numeric_options(column));
                builder.add_f64_field(
                    &geo_latitude_field(&column.name),
                    NumericOptions::default().set_fast().set_indexed(),
                );
                builder.add_f64_field(
                    &geo_longitude_field(&column.name),
                    NumericOptions::default().set_fast().set_indexed(),
                );
                builder.add_bytes_field(
                    &geo_coordinate_field(&column.name),
                    BytesOptions::default().set_fast(),
                );
            }
        }
        if column.data_type.is_array() && column.data_type != ColumnType::GeoPointArray {
            builder.add_bytes_field(
                &format!("__aq_array_{}", column.name),
                BytesOptions::default().set_fast(),
            );
        }
    }
    builder.build()
}

fn numeric_options(column: &ColumnDef) -> NumericOptions {
    let mut options = NumericOptions::default().set_fast();
    if column.index.indexed {
        options = options.set_indexed();
    }
    if column.index.stored {
        options = options.set_stored();
    }
    options
}

fn bytes_options(column: &ColumnDef) -> BytesOptions {
    let mut options = BytesOptions::default().set_fast();
    if column.index.indexed {
        options = options.set_indexed();
    }
    if column.index.stored {
        options = options.set_stored();
    }
    options
}

fn text_record(record: TextIndexRecord) -> IndexRecordOption {
    match record {
        TextIndexRecord::Basic => IndexRecordOption::Basic,
        TextIndexRecord::Frequencies => IndexRecordOption::WithFreqs,
        TextIndexRecord::Positions => IndexRecordOption::WithFreqsAndPositions,
    }
}

pub(super) fn schema_fields(schema: &Schema, def: &TableDef) -> Result<IndexFields> {
    let mut values = HashMap::new();
    let mut raw = HashMap::new();
    let mut arrays = HashMap::new();
    let mut geo_coordinates = HashMap::new();
    let mut geo_latitudes = HashMap::new();
    let mut geo_longitudes = HashMap::new();
    for column in &def.columns {
        values.insert(column.name.clone(), schema.get_field(&column.name)?);
        if matches!(
            column.data_type,
            ColumnType::Text
                | ColumnType::TextArray
                | ColumnType::Json
                | ColumnType::JsonArray
                | ColumnType::Facet
                | ColumnType::FacetArray
        ) {
            let raw_field = schema
                .get_field(&format!("__aq_raw_{}", column.name))
                .unwrap_or(values[&column.name]);
            raw.insert(column.name.clone(), raw_field);
        }
        if column.data_type.is_array() && column.data_type != ColumnType::GeoPointArray {
            arrays.insert(
                column.name.clone(),
                schema.get_field(&format!("__aq_array_{}", column.name))?,
            );
        }
        if matches!(
            column.data_type,
            ColumnType::GeoPoint | ColumnType::GeoPointArray
        ) {
            geo_coordinates.insert(
                column.name.clone(),
                schema.get_field(&geo_coordinate_field(&column.name))?,
            );
            geo_latitudes.insert(
                column.name.clone(),
                schema.get_field(&geo_latitude_field(&column.name))?,
            );
            geo_longitudes.insert(
                column.name.clone(),
                schema.get_field(&geo_longitude_field(&column.name))?,
            );
        }
    }
    Ok(IndexFields {
        values,
        raw,
        arrays,
        geo_coordinates,
        geo_latitudes,
        geo_longitudes,
    })
}

pub(super) fn register_analyzers(index: &Index, def: &TableDef) -> Result<()> {
    for column in def
        .columns
        .iter()
        .filter(|c| matches!(c.data_type, ColumnType::Text | ColumnType::TextArray))
    {
        match column.analyzer.as_ref().unwrap() {
            Analyzer::Stem(name) => {
                let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(RemoveLongFilter::limit(40))
                    .filter(LowerCaser)
                    .filter(Stemmer::new(language(name)?))
                    .build();
                index
                    .tokenizers()
                    .register(&analyzer_name(column), analyzer);
            }
            Analyzer::Ngram {
                min,
                max,
                prefix_only,
            } => {
                index.tokenizers().register(
                    &analyzer_name(column),
                    NgramTokenizer::new(*min, *max, *prefix_only)?,
                );
            }
            Analyzer::Custom {
                stem,
                stop_words,
                synonyms,
                ascii_folding,
            } => {
                let mut builder = TextAnalyzer::builder(SimpleTokenizer::default())
                    .filter(RemoveLongFilter::limit(40))
                    .filter(LowerCaser)
                    .dynamic();
                if *ascii_folding {
                    builder = builder.filter_dynamic(AsciiFoldingFilter);
                }
                if !stop_words.is_empty() {
                    builder = builder.filter_dynamic(StopWordFilter::remove(stop_words.clone()));
                }
                if !synonyms.is_empty() {
                    builder = builder.filter_dynamic(SynonymFilter::new(synonyms.clone()));
                }
                if let Some(language_name) = stem {
                    builder = builder.filter_dynamic(Stemmer::new(language(language_name)?));
                }
                index
                    .tokenizers()
                    .register(&analyzer_name(column), builder.build());
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn add_sql_value(
    doc: &mut TantivyDocument,
    fields: &IndexFields,
    column: &ColumnDef,
    value: ValueRef<'_>,
) -> Result<()> {
    let field = fields.values[&column.name];
    if value == ValueRef::Null {
        return Ok(());
    }
    match (column.data_type, value) {
        (ColumnType::Integer, ValueRef::Integer(v)) => doc.add_i64(field, v),
        (ColumnType::Unsigned, ValueRef::Text(v)) => {
            doc.add_u64(field, std::str::from_utf8(v)?.parse()?);
        }
        (ColumnType::Real, ValueRef::Real(v)) => doc.add_f64(field, v),
        (ColumnType::Real, ValueRef::Integer(v)) => doc.add_f64(field, v as f64),
        (ColumnType::Boolean, ValueRef::Integer(v @ 0..=1)) => doc.add_bool(field, v == 1),
        (ColumnType::Date, ValueRef::Text(v)) => {
            let text = std::str::from_utf8(v)?;
            let date = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .with_context(|| format!("invalid DATE in {}: {text}", column.name))?;
            let timestamp = date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros();
            doc.add_date(field, DateTime::from_timestamp_micros(timestamp));
        }
        (ColumnType::DateTime, ValueRef::Text(v)) => {
            let text = std::str::from_utf8(v)?;
            let timestamp = ChronoDateTime::parse_from_rfc3339(text)
                .with_context(|| format!("invalid RFC3339 DATETIME in {}: {text}", column.name))?
                .timestamp_micros();
            doc.add_date(field, DateTime::from_timestamp_micros(timestamp));
        }
        (ColumnType::Timestamp, ValueRef::Text(v)) => {
            let text = std::str::from_utf8(v)?;
            doc.add_date(field, timestamp_to_tantivy(text)?);
        }
        (ColumnType::Blob, ValueRef::Blob(v)) => doc.add_bytes(field, v),
        (ColumnType::Ip, ValueRef::Text(v)) => {
            let ip: std::net::IpAddr = std::str::from_utf8(v)?.parse()?;
            doc.add_ip_addr(field, ip.into_ipv6_addr());
        }
        (ColumnType::Json, ValueRef::Text(v)) => {
            let value: serde_json::Value = serde_json::from_slice(v)?;
            doc.add_field_value(field, &OwnedValue::from(value));
            doc.add_bytes(fields.raw[&column.name], v);
        }
        (ColumnType::Facet, ValueRef::Text(v)) => {
            let value = std::str::from_utf8(v)?;
            doc.add_facet(field, value);
            doc.add_bytes(fields.raw[&column.name], v);
        }
        (ColumnType::Text, ValueRef::Text(v)) => {
            let text = std::str::from_utf8(v)?;
            doc.add_text(field, text);
            if fields.raw[&column.name] != field {
                doc.add_text(fields.raw[&column.name], text);
            }
        }
        (ColumnType::GeoPoint | ColumnType::GeoPointArray, ValueRef::Text(v)) => {
            let points = if column.data_type == ColumnType::GeoPoint {
                vec![serde_json::from_slice(v)?]
            } else {
                serde_json::from_slice(v)?
            };
            index_geo_points(doc, fields, column, &points);
        }
        (ColumnType::TextArray, ValueRef::Text(v)) => {
            let values: Vec<String> = serde_json::from_slice(v)
                .with_context(|| format!("invalid TEXT[] JSON in {}", column.name))?;
            for value in &values {
                doc.add_text(field, value);
                if fields.raw[&column.name] != field {
                    doc.add_text(fields.raw[&column.name], value);
                }
            }
            doc.add_bytes(fields.arrays[&column.name], v);
        }
        (ColumnType::IntegerArray, ValueRef::Text(v)) => {
            let values: Vec<i64> = serde_json::from_slice(v)
                .with_context(|| format!("invalid INTEGER[] JSON in {}", column.name))?;
            for value in values {
                doc.add_i64(field, value);
            }
            doc.add_bytes(fields.arrays[&column.name], v);
        }
        (data_type, ValueRef::Text(v)) if data_type.is_array() => {
            add_sql_array(doc, fields, column, v)?;
        }
        _ => bail!(
            "value in column {} does not match its declared type",
            column.name
        ),
    }
    Ok(())
}

pub(super) fn add_row_value(
    doc: &mut TantivyDocument,
    fields: &IndexFields,
    column: &ColumnDef,
    value: &RowValue,
) -> Result<()> {
    let field = fields.values[&column.name];
    if matches!(value, RowValue::Null) {
        return Ok(());
    }
    match (&column.data_type, value) {
        (ColumnType::Integer, RowValue::Integer(value)) => doc.add_i64(field, *value),
        (ColumnType::Unsigned, RowValue::Unsigned(value)) => doc.add_u64(field, *value),
        (ColumnType::Real, RowValue::Real(value)) => doc.add_f64(field, *value),
        (ColumnType::Real, RowValue::Integer(value)) => doc.add_f64(field, *value as f64),
        (ColumnType::Boolean, RowValue::Integer(value @ 0..=1)) => doc.add_bool(field, *value == 1),
        (ColumnType::Date, RowValue::Text(value)) => {
            let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .with_context(|| format!("invalid DATE in {}: {value}", column.name))?;
            let timestamp = date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros();
            doc.add_date(field, DateTime::from_timestamp_micros(timestamp));
        }
        (ColumnType::DateTime, RowValue::Text(value)) => {
            let timestamp = ChronoDateTime::parse_from_rfc3339(value)
                .with_context(|| format!("invalid RFC3339 DATETIME in {}: {value}", column.name))?
                .timestamp_micros();
            doc.add_date(field, DateTime::from_timestamp_micros(timestamp));
        }
        (ColumnType::Timestamp, RowValue::Text(value)) => {
            doc.add_date(field, timestamp_to_tantivy(value)?);
        }
        (ColumnType::Blob, RowValue::Blob(value)) => doc.add_bytes(field, value),
        (ColumnType::Ip, RowValue::Text(value)) => {
            let ip: std::net::IpAddr = value.parse()?;
            doc.add_ip_addr(field, ip.into_ipv6_addr());
        }
        (ColumnType::Json, RowValue::Json(value)) => {
            doc.add_field_value(field, &OwnedValue::from(value.clone()));
            doc.add_bytes(fields.raw[&column.name], &serde_json::to_vec(value)?);
        }
        (ColumnType::Facet, RowValue::Text(value)) => {
            doc.add_facet(field, value);
            doc.add_bytes(fields.raw[&column.name], value.as_bytes());
        }
        (ColumnType::Text, RowValue::Text(value)) => {
            doc.add_text(field, value);
            if fields.raw[&column.name] != field {
                doc.add_text(fields.raw[&column.name], value);
            }
        }
        (ColumnType::GeoPoint, RowValue::GeoPoint(point)) => {
            index_geo_points(doc, fields, column, std::slice::from_ref(point));
        }
        (ColumnType::GeoPointArray, RowValue::GeoPointArray(points)) => {
            index_geo_points(doc, fields, column, points);
        }
        (ColumnType::TextArray, RowValue::TextArray(values)) => {
            for value in values {
                doc.add_text(field, value);
                if fields.raw[&column.name] != field {
                    doc.add_text(fields.raw[&column.name], value);
                }
            }
            let encoded = serde_json::to_vec(values).expect("serializable array");
            doc.add_bytes(fields.arrays[&column.name], &encoded);
        }
        (ColumnType::IntegerArray, RowValue::IntegerArray(values)) => {
            for value in values {
                doc.add_i64(field, *value);
            }
            let encoded = serde_json::to_vec(values).expect("serializable array");
            doc.add_bytes(fields.arrays[&column.name], &encoded);
        }
        (data_type, value) if data_type.is_array() => {
            add_row_array(doc, fields, column, value)?;
        }
        _ => bail!(
            "outbox value in column {} does not match its declared type",
            column.name
        ),
    }
    Ok(())
}

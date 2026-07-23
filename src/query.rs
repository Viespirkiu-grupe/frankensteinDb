use super::*;

#[derive(Clone)]
pub(super) struct ResultRow {
    pub(super) values: Vec<Value>,
    pub(super) columns: Arc<HashMap<String, usize>>,
    pub(super) score: f64,
}

mod highlight;
pub(crate) use highlight::*;
mod json_path;
pub(crate) use json_path::*;

pub(super) struct ProjectedFastReader {
    pub(super) data_type: ColumnType,
    pub(super) values: FastValues,
}

#[derive(Clone)]
pub(super) enum FastValues {
    Integer(Column<i64>),
    Unsigned(Column<u64>),
    Real(Column<f64>),
    Text(StrColumn),
    Boolean(Column<bool>),
    Date(Column<DateTime>),
    Ip(Column<std::net::Ipv6Addr>),
    Blob(BytesColumn),
    Array(BytesColumn),
    Json(BytesColumn),
    Facet(BytesColumn),
    Geo(BytesColumn),
    GeoDistance {
        coordinates: BytesColumn,
        origin: GeoPoint,
        mode: GeoDistanceMode,
    },
}

impl FastValues {
    fn segment_sort_value(&self, doc_id: DocId) -> Option<u64> {
        match self {
            Self::Integer(values) => values.first(doc_id).map(i64::to_u64),
            Self::Unsigned(values) => values.first(doc_id),
            Self::Real(values) => values.first(doc_id).map(f64::to_u64),
            Self::Text(values) => values.term_ords(doc_id).next(),
            Self::Boolean(values) => values.first(doc_id).map(bool::to_u64),
            Self::Date(values) => values.first(doc_id).map(DateTime::to_u64),
            Self::Ip(_) => None,
            Self::Geo(_) => None,
            Self::GeoDistance {
                coordinates,
                origin,
                mode,
            } => geo_distance_value(coordinates, doc_id, *origin, *mode).map(f64::to_u64),
            Self::Blob(values) | Self::Array(values) | Self::Json(values) | Self::Facet(values) => {
                values.ords().first(doc_id)
            }
        }
    }

    fn global_sort_value(&self, value: Option<u64>) -> OwnedValue {
        let Some(value) = value else {
            return OwnedValue::Null;
        };
        match self {
            Self::Integer(_) => OwnedValue::I64(i64::from_u64(value)),
            Self::Unsigned(_) => OwnedValue::U64(value),
            Self::Real(_) => OwnedValue::F64(f64::from_u64(value)),
            Self::Boolean(_) => OwnedValue::Bool(bool::from_u64(value)),
            Self::Date(_) => OwnedValue::Date(DateTime::from_u64(value)),
            Self::Ip(_) => OwnedValue::Null,
            Self::Geo(_) => OwnedValue::Null,
            Self::GeoDistance { .. } => OwnedValue::F64(f64::from_u64(value)),
            Self::Text(values) => {
                let mut output = String::new();
                values
                    .ord_to_str(value, &mut output)
                    .expect("valid text fast-field dictionary");
                OwnedValue::Str(output)
            }
            Self::Blob(values) | Self::Array(values) | Self::Json(values) | Self::Facet(values) => {
                let mut output = Vec::new();
                values
                    .ord_to_bytes(value, &mut output)
                    .expect("valid bytes fast-field dictionary");
                OwnedValue::Bytes(output)
            }
        }
    }

    fn sort_owned_value(&self, doc_id: DocId) -> OwnedValue {
        match self {
            Self::Ip(values) => values
                .first(doc_id)
                .map(OwnedValue::IpAddr)
                .unwrap_or(OwnedValue::Null),
            _ => self.global_sort_value(self.segment_sort_value(doc_id)),
        }
    }
}

impl ProjectedFastReader {
    pub(super) fn value(&self, doc_id: u32) -> Result<Value> {
        let value = match &self.values {
            FastValues::Integer(values) => values.first(doc_id).map(OwnedValue::I64),
            FastValues::Unsigned(values) => values.first(doc_id).map(OwnedValue::U64),
            FastValues::Real(values) => values.first(doc_id).map(OwnedValue::F64),
            FastValues::Boolean(values) => values.first(doc_id).map(OwnedValue::Bool),
            FastValues::Date(values) => values.first(doc_id).map(OwnedValue::Date),
            FastValues::Ip(values) => values.first(doc_id).map(OwnedValue::IpAddr),
            FastValues::Text(values) => {
                let Some(ord) = values.term_ords(doc_id).next() else {
                    return Ok(Value::Null);
                };
                let mut output = String::new();
                ensure!(values.ord_to_str(ord, &mut output)?, "missing text ordinal");
                Some(OwnedValue::Str(output))
            }
            FastValues::Blob(values) => {
                let Some(ord) = values.ords().first(doc_id) else {
                    return Ok(Value::Null);
                };
                let mut output = Vec::new();
                ensure!(
                    values.ord_to_bytes(ord, &mut output)?,
                    "missing BLOB ordinal"
                );
                Some(OwnedValue::Bytes(output))
            }
            FastValues::Array(values) => {
                let Some(ord) = values.ords().first(doc_id) else {
                    return Ok(Value::Null);
                };
                let mut output = Vec::new();
                ensure!(
                    values.ord_to_bytes(ord, &mut output)?,
                    "missing array ordinal"
                );
                return Ok(serde_json::from_slice(&output)?);
            }
            FastValues::Json(values) => {
                let Some(ord) = values.ords().first(doc_id) else {
                    return Ok(Value::Null);
                };
                let mut output = Vec::new();
                ensure!(values.ord_to_bytes(ord, &mut output)?, "missing JSON value");
                return Ok(serde_json::from_slice(&output)?);
            }
            FastValues::Facet(values) => {
                let Some(ord) = values.ords().first(doc_id) else {
                    return Ok(Value::Null);
                };
                let mut output = Vec::new();
                ensure!(
                    values.ord_to_bytes(ord, &mut output)?,
                    "missing facet value"
                );
                return Ok(Value::String(String::from_utf8(output)?));
            }
            FastValues::Geo(values) => {
                let points = geo_points_value(values, doc_id)?;
                return Ok(if self.data_type == ColumnType::GeoPoint {
                    points.first().map_or(Value::Null, |point| json!(point))
                } else {
                    json!(points)
                });
            }
            FastValues::GeoDistance { .. } => unreachable!("distance reader is sort-only"),
        };
        value
            .map(|value| owned_to_json(value, &self.data_type))
            .transpose()
            .map(|value| value.unwrap_or(Value::Null))
    }
}

pub(super) fn segment_fast_readers(
    searcher: &Searcher,
    segment_ord: u32,
    columns: &[&ColumnDef],
) -> Result<Vec<ProjectedFastReader>> {
    let fast_fields = searcher.segment_reader(segment_ord).fast_fields();
    columns
        .iter()
        .map(|column| {
            let field = if column.data_type.is_array() {
                format!("__aq_array_{}", column.name)
            } else if matches!(column.data_type, ColumnType::Json | ColumnType::Facet) {
                format!("__aq_raw_{}", column.name)
            } else {
                aggregation_field(column)
            };
            let values = match column.data_type {
                ColumnType::Integer => FastValues::Integer(fast_fields.i64(&field)?),
                ColumnType::Unsigned => FastValues::Unsigned(fast_fields.u64(&field)?),
                ColumnType::Real => FastValues::Real(fast_fields.f64(&field)?),
                ColumnType::Text => FastValues::Text(
                    fast_fields
                        .str(&field)?
                        .ok_or_else(|| anyhow!("missing text fast field: {field}"))?,
                ),
                ColumnType::Boolean => FastValues::Boolean(fast_fields.bool(&field)?),
                ColumnType::Date | ColumnType::DateTime | ColumnType::Timestamp => {
                    FastValues::Date(fast_fields.date(&field)?)
                }
                ColumnType::Ip => FastValues::Ip(fast_fields.ip_addr(&field)?),
                ColumnType::GeoPoint | ColumnType::GeoPointArray => FastValues::Geo(
                    fast_fields
                        .bytes(&geo_coordinate_field(&column.name))?
                        .ok_or_else(|| anyhow!("missing geo fast field: {}", column.name))?,
                ),
                data_type if data_type.is_array() => FastValues::Array(
                    fast_fields
                        .bytes(&field)?
                        .ok_or_else(|| anyhow!("missing array fast field: {field}"))?,
                ),
                ColumnType::Blob => FastValues::Blob(
                    fast_fields
                        .bytes(&field)?
                        .ok_or_else(|| anyhow!("missing BLOB fast field: {field}"))?,
                ),
                ColumnType::Json => FastValues::Json(
                    fast_fields
                        .bytes(&field)?
                        .ok_or_else(|| anyhow!("missing raw fast field: {field}"))?,
                ),
                ColumnType::Facet => FastValues::Facet(
                    fast_fields
                        .bytes(&field)?
                        .ok_or_else(|| anyhow!("missing raw fast field: {field}"))?,
                ),
                _ => unreachable!("array types are handled by the guarded arm"),
            };
            Ok(ProjectedFastReader {
                data_type: column.data_type,
                values,
            })
        })
        .collect()
}

#[derive(Clone)]
pub(super) struct OrderSpec {
    pub(super) key: String,
    pub(super) json_type: Option<JsonPathType>,
    pub(super) asc: bool,
    pub(super) geo_distance_from: Option<GeoPoint>,
    pub(super) geo_distance_mode: GeoDistanceMode,
}

pub(super) fn geo_points_value(values: &BytesColumn, doc_id: DocId) -> Result<Vec<GeoPoint>> {
    let Some(ord) = values.ords().first(doc_id) else {
        return Ok(Vec::new());
    };
    let mut encoded = Vec::new();
    ensure!(
        values.ord_to_bytes(ord, &mut encoded)?,
        "missing geo coordinate ordinal"
    );
    decode_points(&encoded)
}

pub(super) fn geo_distance_value(
    values: &BytesColumn,
    doc_id: DocId,
    origin: GeoPoint,
    mode: GeoDistanceMode,
) -> Option<f64> {
    geo_distance_value_with_buffer(values, doc_id, origin, mode, &mut Vec::new())
}

pub(super) fn geo_distance_value_with_buffer(
    values: &BytesColumn,
    doc_id: DocId,
    origin: GeoPoint,
    mode: GeoDistanceMode,
    encoded: &mut Vec<u8>,
) -> Option<f64> {
    encoded.clear();
    let ord = values.ords().first(doc_id)?;
    if values.ord_to_bytes(ord, encoded).ok() != Some(true) {
        return None;
    }
    distance_for_encoded_points(encoded, origin, mode)
        .ok()
        .flatten()
}

mod advanced_text;
mod aggregation;
mod block_top_k;
mod compiler;
mod cursor;
mod filter_score;
mod projection;
mod scored_top_k;
mod scoring;
mod typed_aggregation;
mod typed_compiler;
mod typed_projection;
mod typed_values;

pub(super) use advanced_text::*;
pub(super) use aggregation::*;
use block_top_k::collect_block_top_k;
pub(super) use compiler::*;
pub(super) use cursor::*;
use filter_score::*;
pub(super) use projection::*;
pub(super) use scored_top_k::*;
pub(super) use scoring::*;
pub(super) use typed_aggregation::*;
pub(super) use typed_compiler::*;
pub(super) use typed_projection::*;
pub(super) use typed_values::*;

use anyhow::{Result, bail};
use chrono::DateTime as ChronoDateTime;
use tantivy::DateTime;
use tantivy::schema::{IntoIpv6Addr, OwnedValue, TantivyDocument};

use crate::model::RowValue;
use crate::{ColumnDef, ColumnType, IndexFields, parse_timestamp};

/// Adds a SQLite-encoded array as native repeated Tantivy values plus its canonical fast value.
pub(super) fn add_sql_array(
    doc: &mut TantivyDocument,
    fields: &IndexFields,
    column: &ColumnDef,
    encoded: &[u8],
) -> Result<()> {
    let field = fields.values[&column.name];
    match column.data_type {
        ColumnType::UnsignedArray => {
            for value in serde_json::from_slice::<Vec<u64>>(encoded)? {
                doc.add_u64(field, value);
            }
        }
        ColumnType::RealArray => {
            for value in serde_json::from_slice::<Vec<f64>>(encoded)? {
                doc.add_f64(field, value);
            }
        }
        ColumnType::BooleanArray => {
            for value in serde_json::from_slice::<Vec<bool>>(encoded)? {
                doc.add_bool(field, value);
            }
        }
        ColumnType::DateArray | ColumnType::DateTimeArray | ColumnType::TimestampArray => {
            for value in serde_json::from_slice::<Vec<String>>(encoded)? {
                doc.add_date(field, array_date(&column.data_type, &value)?);
            }
        }
        ColumnType::BlobArray => {
            for value in serde_json::from_slice::<Vec<String>>(encoded)? {
                doc.add_bytes(
                    field,
                    &hex::decode(value.strip_prefix("0x").unwrap_or(&value))?,
                );
            }
        }
        ColumnType::IpArray => {
            for value in serde_json::from_slice::<Vec<String>>(encoded)? {
                doc.add_ip_addr(field, value.parse::<std::net::IpAddr>()?.into_ipv6_addr());
            }
        }
        ColumnType::JsonArray => {
            for value in serde_json::from_slice::<Vec<serde_json::Value>>(encoded)? {
                doc.add_field_value(field, &OwnedValue::from(value));
            }
        }
        ColumnType::FacetArray => {
            for value in serde_json::from_slice::<Vec<String>>(encoded)? {
                doc.add_facet(field, &value);
            }
        }
        _ => bail!("unsupported array type in {}", column.name),
    }
    doc.add_bytes(fields.arrays[&column.name], encoded);
    Ok(())
}

/// Adds an outbox array without converting it back through SQLite.
pub(super) fn add_row_array(
    doc: &mut TantivyDocument,
    fields: &IndexFields,
    column: &ColumnDef,
    value: &RowValue,
) -> Result<()> {
    let encoded = match (&column.data_type, value) {
        (ColumnType::UnsignedArray, RowValue::UnsignedArray(values)) => serde_json::to_vec(values)?,
        (ColumnType::RealArray, RowValue::RealArray(values)) => serde_json::to_vec(values)?,
        (ColumnType::BooleanArray, RowValue::BooleanArray(values)) => serde_json::to_vec(values)?,
        (
            ColumnType::DateArray
            | ColumnType::DateTimeArray
            | ColumnType::TimestampArray
            | ColumnType::IpArray
            | ColumnType::FacetArray,
            RowValue::TextArray(values),
        ) => serde_json::to_vec(values)?,
        (ColumnType::BlobArray, RowValue::BlobArray(values)) => serde_json::to_vec(
            &values
                .iter()
                .map(|value| format!("0x{}", hex::encode(value)))
                .collect::<Vec<_>>(),
        )?,
        (ColumnType::JsonArray, RowValue::JsonArray(values)) => serde_json::to_vec(values)?,
        _ => bail!("outbox array in {} has the wrong type", column.name),
    };
    add_sql_array(doc, fields, column, &encoded)
}

fn array_date(data_type: &ColumnType, value: &str) -> Result<DateTime> {
    let micros = match data_type {
        ColumnType::DateArray => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_micros(),
        ColumnType::DateTimeArray => ChronoDateTime::parse_from_rfc3339(value)?.timestamp_micros(),
        ColumnType::TimestampArray => parse_timestamp(value)?.and_utc().timestamp_micros(),
        _ => unreachable!(),
    };
    Ok(DateTime::from_timestamp_micros(micros))
}

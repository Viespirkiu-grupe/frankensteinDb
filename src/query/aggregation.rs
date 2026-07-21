use super::*;

pub(crate) fn aggregation_field(column: &ColumnDef) -> String {
    if matches!(column.data_type, ColumnType::Text | ColumnType::TextArray) {
        if column.compact_raw {
            column.name.clone()
        } else {
            format!("__aq_raw_{}", column.name)
        }
    } else {
        column.name.clone()
    }
}

pub(crate) fn existence_field(column: &ColumnDef) -> String {
    match &column.data_type {
        ColumnType::Text => aggregation_field(column),
        ColumnType::GeoPointArray => column.name.clone(),
        data_type if data_type.is_array() => {
            format!("__aq_array_{}", column.name)
        }
        _ => column.name.clone(),
    }
}

pub(crate) fn normalize_group_key(column: &ColumnDef, key: Value) -> Result<Value> {
    const NULL_SENTINEL: &str = "__frankensteindb_null_group_7f734f5e__";
    if key.as_str() == Some(NULL_SENTINEL) {
        return Ok(Value::Null);
    }
    Ok(match column.data_type {
        ColumnType::Boolean => Value::Bool(key.as_i64().unwrap_or_default() != 0),
        ColumnType::Date => {
            let value = key.as_str().context("non-string DATE bucket")?;
            let date = ChronoDateTime::parse_from_rfc3339(value)?.date_naive();
            Value::String(date.format("%Y-%m-%d").to_string())
        }
        ColumnType::DateTime => {
            let value = key.as_str().context("non-string DATETIME bucket")?;
            Value::String(ChronoDateTime::parse_from_rfc3339(value)?.to_rfc3339())
        }
        ColumnType::Timestamp => {
            let value = key.as_str().context("non-string TIMESTAMP bucket")?;
            let parsed = ChronoDateTime::parse_from_rfc3339(value)?;
            Value::String(
                parsed
                    .naive_utc()
                    .format("%Y-%m-%dT%H:%M:%S%.3f")
                    .to_string(),
            )
        }
        _ => key,
    })
}

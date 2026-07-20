use super::*;

/// Converts a public JSON value into the canonical typed mutation value.
pub(crate) fn json_to_row_value(column: &ColumnDef, value: &Value) -> Result<RowValue> {
    let value = match (&column.data_type, value) {
        (_, Value::Null) => RowValue::Null,
        (ColumnType::Integer, Value::Number(value)) => RowValue::Integer(
            value
                .as_i64()
                .ok_or_else(|| anyhow!("column {} requires an integer", column.name))?,
        ),
        (ColumnType::Unsigned, Value::Number(value)) => RowValue::Unsigned(
            value
                .as_u64()
                .ok_or_else(|| anyhow!("column {} requires an unsigned integer", column.name))?,
        ),
        (ColumnType::Real, Value::Number(value)) => {
            if let Some(value) = value.as_i64() {
                RowValue::Integer(value)
            } else {
                RowValue::Real(
                    value
                        .as_f64()
                        .ok_or_else(|| anyhow!("column {} requires a number", column.name))?,
                )
            }
        }
        (ColumnType::Boolean, Value::Bool(value)) => RowValue::Integer(i64::from(*value)),
        (ColumnType::Blob, Value::String(value)) => {
            let encoded = value.strip_prefix("0x").unwrap_or(value);
            RowValue::Blob(hex::decode(encoded).with_context(|| {
                format!("column {} requires hexadecimal BLOB data", column.name)
            })?)
        }
        (
            ColumnType::Text
            | ColumnType::Date
            | ColumnType::DateTime
            | ColumnType::Timestamp
            | ColumnType::Ip
            | ColumnType::Facet,
            Value::String(value),
        ) => RowValue::Text(value.clone()),
        (ColumnType::Json, Value::Object(_)) => RowValue::Json(value.clone()),
        (ColumnType::TextArray, Value::Array(values)) => RowValue::TextArray(
            values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        anyhow!("column {} requires an array of strings", column.name)
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::IntegerArray, Value::Array(values)) => RowValue::IntegerArray(
            values
                .iter()
                .map(|value| {
                    value.as_i64().ok_or_else(|| {
                        anyhow!("column {} requires an array of integers", column.name)
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::UnsignedArray, Value::Array(values)) => RowValue::UnsignedArray(
            values
                .iter()
                .map(|value| {
                    value.as_u64().ok_or_else(|| {
                        anyhow!(
                            "column {} requires an array of unsigned integers",
                            column.name
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::RealArray, Value::Array(values)) => RowValue::RealArray(
            values
                .iter()
                .map(|value| {
                    value.as_f64().ok_or_else(|| {
                        anyhow!("column {} requires an array of numbers", column.name)
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::BooleanArray, Value::Array(values)) => RowValue::BooleanArray(
            values
                .iter()
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        anyhow!("column {} requires an array of booleans", column.name)
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (
            ColumnType::DateArray
            | ColumnType::DateTimeArray
            | ColumnType::TimestampArray
            | ColumnType::IpArray
            | ColumnType::FacetArray,
            Value::Array(values),
        ) => RowValue::TextArray(
            values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        anyhow!("column {} requires an array of strings", column.name)
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::BlobArray, Value::Array(values)) => RowValue::BlobArray(
            values
                .iter()
                .map(|value| {
                    let value = value.as_str().ok_or_else(|| {
                        anyhow!(
                            "column {} requires an array of hexadecimal strings",
                            column.name
                        )
                    })?;
                    hex::decode(value.strip_prefix("0x").unwrap_or(value)).map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        (ColumnType::JsonArray, Value::Array(values)) => {
            ensure!(
                values.iter().all(Value::is_object),
                "column {} requires an array of JSON objects",
                column.name
            );
            RowValue::JsonArray(values.clone())
        }
        _ => bail!("JSON value does not match type of column {}", column.name),
    };
    validate_row_value(column, &value)?;
    Ok(value)
}

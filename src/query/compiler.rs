use super::*;

pub(crate) fn negate(query: Box<dyn Query>) -> Result<Box<dyn Query>> {
    Ok(Box::new(BooleanQuery::new(vec![
        (Occur::Must, Box::new(AllQuery)),
        (Occur::MustNot, query),
    ])))
}

pub(crate) fn term_query(term: Term) -> Box<dyn Query> {
    Box::new(TermQuery::new(term, IndexRecordOption::Basic))
}

pub(crate) fn column<'a>(def: &'a TableDef, name: &str) -> Result<&'a ColumnDef> {
    def.columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("unknown column: {name}"))
}

pub(crate) fn owned_to_json(value: OwnedValue, column_type: &ColumnType) -> Result<Value> {
    Ok(match value {
        OwnedValue::Null => Value::Null,
        OwnedValue::Str(value) => Value::String(value),
        OwnedValue::U64(value) => json!(value),
        OwnedValue::I64(value) => json!(value),
        OwnedValue::F64(value) => Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        OwnedValue::Bool(value) => json!(value),
        OwnedValue::IpAddr(value) => Value::String(
            value
                .to_ipv4_mapped()
                .map_or_else(|| value.to_string(), |ip| ip.to_string()),
        ),
        OwnedValue::Date(value) => {
            let datetime =
                ChronoDateTime::<Utc>::from_timestamp_micros(value.into_timestamp_micros())
                    .context("stored datetime is out of range")?;
            if *column_type == ColumnType::Date {
                Value::String(datetime.format("%Y-%m-%d").to_string())
            } else if *column_type == ColumnType::Timestamp {
                Value::String(
                    datetime
                        .naive_utc()
                        .format("%Y-%m-%dT%H:%M:%S%.3f")
                        .to_string(),
                )
            } else {
                Value::String(datetime.to_rfc3339())
            }
        }
        OwnedValue::Bytes(value) if matches!(column_type, ColumnType::Json) => {
            serde_json::from_slice(&value)?
        }
        OwnedValue::Bytes(value) if matches!(column_type, ColumnType::Facet) => {
            Value::String(String::from_utf8(value)?)
        }
        OwnedValue::Bytes(value) => Value::String(format!("0x{}", hex::encode(value))),
        other => Value::String(format!("{other:?}")),
    })
}

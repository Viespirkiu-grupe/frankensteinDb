use super::*;

pub(super) fn compile_typed_comparison(
    def: &TableDef,
    fields: &IndexFields,
    name: &str,
    operator: Comparison,
    value: &Value,
) -> Result<Box<dyn Query>> {
    let term = term_for_value(fields, column(def, name)?, value)?;
    match operator {
        Comparison::Equal => Ok(term_query(term)),
        Comparison::NotEqual => negate(term_query(term)),
        Comparison::Greater => Ok(Box::new(RangeQuery::new(
            Bound::Excluded(term),
            Bound::Unbounded,
        ))),
        Comparison::GreaterOrEqual => Ok(Box::new(RangeQuery::new(
            Bound::Included(term),
            Bound::Unbounded,
        ))),
        Comparison::Less => Ok(Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Excluded(term),
        ))),
        Comparison::LessOrEqual => Ok(Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Included(term),
        ))),
    }
}

pub(crate) fn term_for_value(
    fields: &IndexFields,
    column: &ColumnDef,
    value: &Value,
) -> Result<Term> {
    let field = if matches!(column.data_type, ColumnType::Text | ColumnType::TextArray) {
        fields.raw[&column.name]
    } else {
        fields.values[&column.name]
    };
    Ok(match (&column.data_type, value) {
        (ColumnType::Integer | ColumnType::IntegerArray, Value::Number(value)) => {
            Term::from_field_i64(field, value.as_i64().context("expected integer")?)
        }
        (ColumnType::Unsigned | ColumnType::UnsignedArray, Value::Number(value)) => {
            Term::from_field_u64(field, value.as_u64().context("expected unsigned integer")?)
        }
        (ColumnType::Real | ColumnType::RealArray, Value::Number(value)) => {
            Term::from_field_f64(field, value.as_f64().context("expected number")?)
        }
        (ColumnType::Boolean | ColumnType::BooleanArray, Value::Bool(value)) => {
            Term::from_field_bool(field, *value)
        }
        (ColumnType::Text | ColumnType::TextArray, Value::String(value)) => {
            Term::from_field_text(field, value)
        }
        (ColumnType::Date | ColumnType::DateArray, Value::String(value)) => {
            let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
            let micros = date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros();
            Term::from_field_date_for_search(field, DateTime::from_timestamp_micros(micros))
        }
        (ColumnType::DateTime | ColumnType::DateTimeArray, Value::String(value)) => {
            let micros = ChronoDateTime::parse_from_rfc3339(value)?.timestamp_micros();
            Term::from_field_date_for_search(field, DateTime::from_timestamp_micros(micros))
        }
        (ColumnType::Timestamp | ColumnType::TimestampArray, Value::String(value)) => {
            Term::from_field_date_for_search(field, timestamp_to_tantivy(value)?)
        }
        (ColumnType::Blob | ColumnType::BlobArray, Value::String(value)) => Term::from_field_bytes(
            field,
            &hex::decode(value.strip_prefix("0x").unwrap_or(value))?,
        ),
        (ColumnType::Ip | ColumnType::IpArray, Value::String(value)) => {
            Term::from_field_ip_addr(field, value.parse::<std::net::IpAddr>()?.into_ipv6_addr())
        }
        (ColumnType::Facet | ColumnType::FacetArray, Value::String(value)) => {
            Term::from_facet(field, &Facet::from(value.as_str()))
        }
        (ColumnType::Json | ColumnType::JsonArray, _) => {
            bail!("JSON comparison requires a full-text JSON-path query")
        }
        (ColumnType::GeoPoint | ColumnType::GeoPointArray, _) => {
            bail!("GEO_POINT comparison requires a geo filter")
        }
        _ => bail!("value does not match type of column {}", column.name),
    })
}

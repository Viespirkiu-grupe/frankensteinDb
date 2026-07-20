use super::*;

pub(super) fn convert_bounds(
    column: &ColumnDef,
    bounds: Option<&HistogramBounds>,
) -> Result<Option<Value>> {
    bounds
        .map(|bounds| {
            Ok(json!({
                "min": aggregation_bound(column, &bounds.min)?,
                "max": aggregation_bound(column, &bounds.max)?
            }))
        })
        .transpose()
}

pub(super) fn json_bounds(bounds: Option<&HistogramBounds>) -> Result<Option<Value>> {
    bounds
        .map(|bounds| {
            Ok(json!({"min": numeric_json(&bounds.min)?, "max": numeric_json(&bounds.max)?}))
        })
        .transpose()
}

pub(super) fn numeric_json(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite())
        .context("aggregation value must be finite numeric")
}

pub(super) fn typed_missing(column: &ColumnDef, value: &Value) -> Result<Value> {
    match column.data_type {
        ColumnType::Boolean | ColumnType::BooleanArray => Ok(json!(u64::from(
            value.as_bool().context("boolean missing value required")?
        ))),
        _ => Ok(value.clone()),
    }
}

pub(super) fn aggregation_bound(column: &ColumnDef, value: &Value) -> Result<f64> {
    match column.data_type {
        ColumnType::Integer
        | ColumnType::IntegerArray
        | ColumnType::Unsigned
        | ColumnType::UnsignedArray
        | ColumnType::Real
        | ColumnType::RealArray => numeric_json(value),
        ColumnType::Date | ColumnType::DateArray => Ok(chrono::NaiveDate::parse_from_str(
            value.as_str().context("DATE bound must be a string")?,
            "%Y-%m-%d",
        )?
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis() as f64),
        ColumnType::DateTime | ColumnType::DateTimeArray => Ok(ChronoDateTime::parse_from_rfc3339(
            value.as_str().context("DATETIME bound must be a string")?,
        )?
        .timestamp_millis() as f64),
        ColumnType::Timestamp | ColumnType::TimestampArray => Ok(parse_timestamp(
            value.as_str().context("TIMESTAMP bound must be a string")?,
        )?
        .and_utc()
        .timestamp_millis()
            as f64),
        _ => bail!("aggregation bound requires a numeric or date column"),
    }
}

pub(super) fn aggregation_filter_query(def: &TableDef, filter: &Filter) -> Result<String> {
    Ok(match filter {
        Filter::Compare {
            column: name,
            operator,
            value,
        } => {
            column(def, name)?;
            comparison_query(name, *operator, value)
        }
        Filter::Between {
            column: name,
            lower,
            upper,
        } => {
            column(def, name)?;
            between_query(name, lower, upper)
        }
        Filter::JsonCompare {
            column,
            path,
            operator,
            value,
            ..
        } => comparison_query(&json_path_field(def, column, path)?, *operator, value),
        Filter::JsonBetween {
            column,
            path,
            lower,
            upper,
            ..
        } => between_query(&json_path_field(def, column, path)?, lower, upper),
        Filter::JsonExists {
            column,
            path,
            negated,
            ..
        } => {
            let query = format!(
                "{}:*",
                escape_query_name(&json_path_field(def, column, path)?)
            );
            if *negated {
                format!("NOT ({query})")
            } else {
                query
            }
        }
        Filter::Search { fields, query } if fields.is_empty() => query.clone(),
        Filter::Search { fields, query } => format!(
            "({})",
            fields
                .iter()
                .map(|field| format!("{}:({query})", escape_query_name(field)))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        Filter::All { filters } => join_filters(def, filters, " AND ")?,
        Filter::Any { filters } => join_filters(def, filters, " OR ")?,
        Filter::Not { filter } => format!("NOT ({})", aggregation_filter_query(def, filter)?),
        Filter::Prefix { column, value } => format!(
            "{}:{}*",
            escape_query_name(column),
            escape_query_text(value)
        ),
        Filter::Fuzzy {
            column,
            value,
            distance,
            ..
        } => format!(
            "{}:{}~{}",
            escape_query_name(column),
            escape_query_text(value),
            distance
        ),
        _ => bail!("this typed filter is not supported inside a filter aggregation"),
    })
}

fn comparison_query(field: &str, operator: Comparison, value: &Value) -> String {
    let field = escape_query_name(field);
    let value = escape_query_value(value);
    match operator {
        Comparison::Equal => format!("{field}:{value}"),
        Comparison::NotEqual => format!("NOT ({field}:{value})"),
        Comparison::Greater => format!("{field}:{{{value} TO *]"),
        Comparison::GreaterOrEqual => format!("{field}:[{value} TO *]"),
        Comparison::Less => format!("{field}:[* TO {value}}}"),
        Comparison::LessOrEqual => format!("{field}:[* TO {value}]"),
    }
}

fn between_query(field: &str, lower: &Value, upper: &Value) -> String {
    format!(
        "{}:[{} TO {}]",
        escape_query_name(field),
        escape_query_value(lower),
        escape_query_value(upper)
    )
}

fn join_filters(def: &TableDef, filters: &[Filter], separator: &str) -> Result<String> {
    ensure!(!filters.is_empty(), "aggregation filter cannot be empty");
    Ok(format!(
        "({})",
        filters
            .iter()
            .map(|filter| aggregation_filter_query(def, filter))
            .collect::<Result<Vec<_>>>()?
            .join(separator)
    ))
}

fn escape_query_name(value: &str) -> String {
    value.replace('\\', "\\\\").replace(':', "\\:")
}

fn escape_query_value(value: &Value) -> String {
    match value {
        Value::String(value) => format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")),
        value => value.to_string(),
    }
}

fn escape_query_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric() || *character == '_')
        .collect()
}

pub(super) fn aggregation_column(def: &TableDef, name: &str) -> Result<String> {
    Ok(aggregation_field(column(def, name)?))
}

pub(super) const fn order_name(descending: bool) -> &'static str {
    if descending { "desc" } else { "asc" }
}

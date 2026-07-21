use super::*;

pub(super) fn ensure_standard_bucket_column(column: &ColumnDef) -> Result<()> {
    ensure!(
        !matches!(
            column.data_type,
            ColumnType::GeoPoint | ColumnType::GeoPointArray
        ),
        "geo columns require geo_tile_grid aggregation"
    );
    Ok(())
}

pub(super) fn top_hits_json(
    def: &TableDef,
    size: usize,
    sort: &[Sort],
    columns: &[String],
    depth: usize,
) -> Result<Value> {
    ensure!(depth > 1, "top_hits must be a sub-aggregation");
    ensure!((1..=100).contains(&size), "top_hits size must be 1..=100");
    let sort = sort
        .iter()
        .map(|sort| {
            ensure!(
                sort.geo_distance_from.is_none(),
                "top_hits does not support geo distance sorting"
            );
            let field = sort
                .json_path
                .as_ref()
                .map(|path| json_path_field(def, &sort.column, path))
                .transpose()?
                .unwrap_or_else(|| sort.column.clone());
            Ok(json!({field: order_name(sort.descending)}))
        })
        .collect::<Result<Vec<_>>>()?;
    let columns = columns
        .iter()
        .map(|name| aggregation_column(def, name))
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({"top_hits": {"size": size, "sort": sort, "docvalue_fields": columns}}))
}

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

pub(super) fn aggregation_column(def: &TableDef, name: &str) -> Result<String> {
    Ok(aggregation_field(column(def, name)?))
}

pub(super) const fn order_name(descending: bool) -> &'static str {
    if descending { "desc" } else { "asc" }
}

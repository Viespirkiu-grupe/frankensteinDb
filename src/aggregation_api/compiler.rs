use super::values::*;
use super::*;

pub(crate) fn compile_aggregations(
    def: &TableDef,
    source: &BTreeMap<String, Aggregation>,
) -> Result<Aggregations> {
    ensure!(
        source.len() <= 100,
        "at most 100 top-level aggregations are allowed"
    );
    let value = source
        .iter()
        .map(|(name, aggregation)| Ok((name.clone(), aggregation_json(def, aggregation, 1)?)))
        .collect::<Result<serde_json::Map<_, _>>>()?;
    Ok(serde_json::from_value(Value::Object(value))?)
}

fn aggregation_json(def: &TableDef, aggregation: &Aggregation, depth: usize) -> Result<Value> {
    ensure!(depth <= 4, "aggregation depth cannot exceed 4");
    let with_children = |kind: &str,
                         body: Value,
                         children: &BTreeMap<String, Aggregation>|
     -> Result<Value> {
        let mut value = serde_json::Map::new();
        value.insert(kind.into(), body);
        if !children.is_empty() {
            let nested = children
                .iter()
                .map(|(name, child)| Ok((name.clone(), aggregation_json(def, child, depth + 1)?)))
                .collect::<Result<serde_json::Map<_, _>>>()?;
            value.insert("aggs".into(), Value::Object(nested));
        }
        Ok(Value::Object(value))
    };
    match aggregation {
        Aggregation::Terms {
            column: name,
            size,
            segment_size,
            min_doc_count,
            missing,
            order,
            aggregations,
        } => {
            ensure!((1..=10_000).contains(size), "terms size must be 1..=10000");
            let column = column(def, name)?;
            let body = terms_body(
                aggregation_field(column),
                *size,
                *segment_size,
                *min_doc_count,
                missing
                    .as_ref()
                    .map(|value| typed_missing(column, value))
                    .transpose()?,
                order.as_ref(),
            )?;
            with_children("terms", body, aggregations)
        }
        Aggregation::JsonTerms {
            target,
            size,
            missing,
            order,
            aggregations,
        } => {
            ensure!((1..=10_000).contains(size), "terms size must be 1..=10000");
            let body = terms_body(
                json_path_field(def, &target.column, &target.path)?,
                *size,
                None,
                None,
                missing.clone(),
                order.as_ref(),
            )?;
            with_children("terms", body, aggregations)
        }
        Aggregation::Histogram {
            column: name,
            interval,
            offset,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => {
            let column = column(def, name)?;
            histogram(
                &with_children,
                aggregation_field(column),
                *interval,
                *offset,
                *min_doc_count,
                convert_bounds(column, hard_bounds.as_ref())?,
                convert_bounds(column, extended_bounds.as_ref())?,
                *keyed,
                aggregations,
            )
        }
        Aggregation::JsonHistogram {
            target,
            interval,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => {
            ensure!(
                matches!(
                    target.data_type,
                    JsonPathType::I64 | JsonPathType::U64 | JsonPathType::F64
                ),
                "JSON histogram requires a numeric path"
            );
            histogram(
                &with_children,
                json_path_field(def, &target.column, &target.path)?,
                *interval,
                None,
                *min_doc_count,
                json_bounds(hard_bounds.as_ref())?,
                json_bounds(extended_bounds.as_ref())?,
                *keyed,
                aggregations,
            )
        }
        Aggregation::DateHistogram {
            column: name,
            fixed_interval,
            offset,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => {
            let column = column(def, name)?;
            ensure!(
                matches!(
                    column.data_type,
                    ColumnType::Date | ColumnType::DateTime | ColumnType::Timestamp
                ),
                "date_histogram requires a date column"
            );
            with_children(
                "date_histogram",
                json!({
                    "field": aggregation_field(column), "fixed_interval": fixed_interval,
                    "offset": offset, "min_doc_count": min_doc_count,
                    "hard_bounds": convert_bounds(column, hard_bounds.as_ref())?,
                    "extended_bounds": convert_bounds(column, extended_bounds.as_ref())?,
                    "keyed": keyed
                }),
                aggregations,
            )
        }
        Aggregation::Range {
            column: name,
            ranges,
            keyed,
            aggregations,
        } => {
            let column = column(def, name)?;
            range_aggregation(
                &with_children,
                aggregation_field(column),
                ranges,
                *keyed,
                |value| aggregation_bound(column, value),
                aggregations,
            )
        }
        Aggregation::JsonRange {
            target,
            ranges,
            keyed,
            aggregations,
        } => {
            ensure!(
                matches!(
                    target.data_type,
                    JsonPathType::I64 | JsonPathType::U64 | JsonPathType::F64
                ),
                "JSON range aggregation requires a numeric path"
            );
            range_aggregation(
                &with_children,
                json_path_field(def, &target.column, &target.path)?,
                ranges,
                *keyed,
                numeric_json,
                aggregations,
            )
        }
        Aggregation::Filter {
            filter,
            aggregations,
        } => with_children(
            "filter",
            json!(aggregation_filter_query(def, filter)?),
            aggregations,
        ),
        Aggregation::Composite {
            sources,
            size,
            after,
            aggregations,
        } => {
            ensure!(
                (1..=10_000).contains(size),
                "composite size must be 1..=10000"
            );
            ensure!(
                !sources.is_empty() && sources.len() <= 16,
                "composite requires 1..=16 sources"
            );
            let sources = sources
                .iter()
                .map(|source| composite_source_json(def, source))
                .collect::<Result<Vec<_>>>()?;
            with_children(
                "composite",
                json!({"sources": sources, "size": size, "after": after}),
                aggregations,
            )
        }
        Aggregation::Metric {
            function,
            column: name,
            json_path,
            percents,
            missing,
        } => metric_json(
            def,
            *function,
            name.as_deref(),
            json_path.as_ref(),
            percents,
            missing.as_ref(),
        ),
        Aggregation::TopHits {
            size,
            sort,
            columns,
        } => top_hits_json(def, *size, sort, columns, depth),
    }
}

fn terms_body(
    field: String,
    size: usize,
    segment_size: Option<usize>,
    min_doc_count: Option<u64>,
    missing: Option<Value>,
    order: Option<&BucketOrder>,
) -> Result<Value> {
    if let Some(segment_size) = segment_size {
        ensure!(
            segment_size >= size && segment_size <= 100_000,
            "segment_size must be size..=100000"
        );
    }
    Ok(json!({
        "field": field, "size": size, "segment_size": segment_size,
        "min_doc_count": min_doc_count, "missing": missing,
        "order": order.map(bucket_order_json)
    }))
}

fn bucket_order_json(order: &BucketOrder) -> Value {
    json!({order.target.clone(): order_name(order.descending)})
}

#[allow(clippy::too_many_arguments)]
fn histogram(
    with_children: &impl Fn(&str, Value, &BTreeMap<String, Aggregation>) -> Result<Value>,
    field: String,
    interval: f64,
    offset: Option<f64>,
    min_doc_count: u64,
    hard_bounds: Option<Value>,
    extended_bounds: Option<Value>,
    keyed: bool,
    children: &BTreeMap<String, Aggregation>,
) -> Result<Value> {
    ensure!(
        interval.is_finite() && interval > 0.0,
        "histogram interval must be positive"
    );
    with_children(
        "histogram",
        json!({"field": field, "interval": interval, "offset": offset,
            "min_doc_count": min_doc_count, "hard_bounds": hard_bounds,
            "extended_bounds": extended_bounds, "keyed": keyed}),
        children,
    )
}

fn range_aggregation(
    with_children: &impl Fn(&str, Value, &BTreeMap<String, Aggregation>) -> Result<Value>,
    field: String,
    ranges: &[AggregationRange],
    keyed: bool,
    bound: impl Fn(&Value) -> Result<f64>,
    children: &BTreeMap<String, Aggregation>,
) -> Result<Value> {
    ensure!(
        !ranges.is_empty() && ranges.len() <= 1_000,
        "range requires 1..=1000 buckets"
    );
    if keyed {
        ensure!(
            ranges.iter().all(|range| range.key.is_some()),
            "keyed ranges require every key"
        );
    }
    let ranges = ranges
        .iter()
        .map(|range| {
            Ok(json!({
                "key": range.key,
                "from": range.from.as_ref().map(&bound).transpose()?,
                "to": range.to.as_ref().map(&bound).transpose()?
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    with_children(
        "range",
        json!({"field": field, "ranges": ranges, "keyed": keyed}),
        children,
    )
}

fn metric_json(
    def: &TableDef,
    function: Metric,
    column_name: Option<&str>,
    json_path: Option<&JsonPath>,
    percents: &Option<Vec<f64>>,
    missing: Option<&Value>,
) -> Result<Value> {
    ensure!(
        column_name.is_none() || json_path.is_none(),
        "metric accepts column or json_path, not both"
    );
    let (field, path_type) = if let Some(path) = json_path {
        (
            json_path_field(def, &path.column, &path.path)?,
            Some(path.data_type),
        )
    } else {
        let name = match (function, column_name) {
            (Metric::Count, None) => &def.columns[primary_key_index(def)].name,
            (_, Some(name)) => name,
            (_, None) => bail!("metric column or json_path is required"),
        };
        (aggregation_column(def, name)?, None)
    };
    if function != Metric::Cardinality
        && function != Metric::Count
        && let Some(path_type) = path_type
    {
        ensure!(
            matches!(
                path_type,
                JsonPathType::I64 | JsonPathType::U64 | JsonPathType::F64
            ),
            "numeric metric requires numeric JSON path"
        );
    }
    let kind = match function {
        Metric::Count => "value_count",
        Metric::Sum => "sum",
        Metric::Average => "avg",
        Metric::Min => "min",
        Metric::Max => "max",
        Metric::Cardinality => "cardinality",
        Metric::Percentiles => "percentiles",
        Metric::Stats => "stats",
        Metric::ExtendedStats => "extended_stats",
    };
    let missing = missing
        .map(|value| -> Result<Value> {
            if matches!(function, Metric::Cardinality | Metric::Count) {
                Ok(value.clone())
            } else {
                Ok(json!(numeric_json(value)?))
            }
        })
        .transpose()?;
    Ok(json!({kind: {"field": field, "percents": percents, "missing": missing}}))
}

fn top_hits_json(
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

fn composite_source_json(def: &TableDef, source: &CompositeSource) -> Result<Value> {
    let (name, kind, body) = match source {
        CompositeSource::Terms {
            name,
            column: field,
            descending,
            missing_bucket,
            missing_order,
        } => (
            name,
            "terms",
            json!({"field": aggregation_column(def, field)?, "order": order_name(*descending), "missing_bucket": missing_bucket, "missing_order": composite_missing_order(*missing_order, *descending)}),
        ),
        CompositeSource::Histogram {
            name,
            column: field,
            interval,
            descending,
            missing_bucket,
            missing_order,
        } => {
            ensure!(
                interval.is_finite() && *interval > 0.0,
                "composite interval must be positive"
            );
            (
                name,
                "histogram",
                json!({"field": aggregation_column(def, field)?, "interval": interval, "order": order_name(*descending), "missing_bucket": missing_bucket, "missing_order": composite_missing_order(*missing_order, *descending)}),
            )
        }
        CompositeSource::DateHistogram {
            name,
            column: field,
            fixed_interval,
            calendar_interval,
            descending,
            missing_bucket,
            missing_order,
        } => {
            ensure!(
                fixed_interval.is_some() ^ calendar_interval.is_some(),
                "composite date histogram requires exactly one interval"
            );
            (
                name,
                "date_histogram",
                json!({"field": aggregation_column(def, field)?, "fixed_interval": fixed_interval, "calendar_interval": calendar_interval, "order": order_name(*descending), "missing_bucket": missing_bucket, "missing_order": composite_missing_order(*missing_order, *descending)}),
            )
        }
        CompositeSource::JsonTerms {
            name,
            target,
            descending,
            missing_bucket,
            missing_order,
        } => (
            name,
            "terms",
            json!({"field": json_path_field(def, &target.column, &target.path)?, "order": order_name(*descending), "missing_bucket": missing_bucket, "missing_order": composite_missing_order(*missing_order, *descending)}),
        ),
    };
    ensure!(!name.is_empty(), "composite source name cannot be empty");
    Ok(json!({name.clone(): {kind: body}}))
}

/// Tantivy's default placement is equivalent to FIRST for ascending sources and LAST for
/// descending sources. Prefer that representation because interval composite sources otherwise
/// treat an absent first-page cursor like an explicit missing cursor for these two combinations.
const fn composite_missing_order(order: MissingOrder, descending: bool) -> MissingOrder {
    match (order, descending) {
        (MissingOrder::First, false) | (MissingOrder::Last, true) => MissingOrder::Default,
        _ => order,
    }
}

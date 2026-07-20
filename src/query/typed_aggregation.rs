use super::*;

struct TypedGroup<'a> {
    column: &'a ColumnDef,
    name: String,
}

struct TypedMetric {
    projection_index: usize,
    name: String,
    function: Aggregate,
}

const NULL_SENTINEL: &str = "__frankensteindb_null_group_7f734f5e__";

pub(crate) fn execute_typed_aggregation(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    request: &ReadRequest,
    order: &[OrderSpec],
) -> Result<QueryResult> {
    ensure!(
        !request.projection.is_empty(),
        "aggregation requires an explicit projection"
    );
    let groups = typed_groups(def, request)?;
    validate_group_projection(request, &groups)?;
    let (metrics, metric_requests) = typed_metrics(def, request)?;
    let bucket_size = typed_bucket_size(request, order, groups.len());
    let aggregations = aggregation_request(&groups, metric_requests, bucket_size)?;
    let collector = AggregationCollector::from_aggs(aggregations, Default::default());
    let result_json = serde_json::to_value(searcher.search(query, &collector)?)?;
    let mut rows = if groups.is_empty() {
        vec![typed_aggregation_row(
            request,
            &groups,
            &metrics,
            &[],
            &result_json,
        )?]
    } else {
        let mut rows = Vec::new();
        collect_typed_group_rows(
            request,
            &groups,
            &metrics,
            0,
            &result_json,
            &mut Vec::new(),
            &mut rows,
        )?;
        rows
    };
    let columns = request
        .projection
        .iter()
        .map(typed_projection_name)
        .collect::<Vec<_>>();
    sort_projected(&mut rows, &columns, order)?;
    let rows = rows
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect::<Vec<_>>();
    Ok(QueryResult {
        message: format!("{} row(s)", rows.len()),
        columns,
        rows,
        next_search_after: None,
    })
}

fn typed_groups<'a>(def: &'a TableDef, request: &ReadRequest) -> Result<Vec<TypedGroup<'a>>> {
    request
        .group_by
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let column = column(def, name)?;
            ensure!(
                !column.data_type.is_array(),
                "array columns cannot be grouped"
            );
            Ok(TypedGroup {
                column,
                name: format!("__aq_group_{index}"),
            })
        })
        .collect()
}

fn validate_group_projection(request: &ReadRequest, groups: &[TypedGroup<'_>]) -> Result<()> {
    for item in &request.projection {
        match item {
            Projection::Column { column, .. } => ensure!(
                groups
                    .iter()
                    .any(|group| group.column.name.eq_ignore_ascii_case(column)),
                "non-aggregate column {column} must be grouped"
            ),
            Projection::Score { .. } => bail!("score cannot be selected with aggregation"),
            Projection::Highlight { .. } => bail!("highlight cannot be selected with aggregation"),
            Projection::Aggregate { .. } => {}
        }
    }
    Ok(())
}

fn typed_metrics(
    def: &TableDef,
    request: &ReadRequest,
) -> Result<(Vec<TypedMetric>, serde_json::Map<String, Value>)> {
    let mut metrics = Vec::new();
    let mut requests = serde_json::Map::new();
    for (index, item) in request.projection.iter().enumerate() {
        let Projection::Aggregate {
            function, column, ..
        } = item
        else {
            continue;
        };
        let aggregate_column = match column {
            Some(name) => column_by_name(def, name)?,
            None if *function == Aggregate::Count => &def.columns[primary_key_index(def)],
            None => bail!("only count may omit its column"),
        };
        ensure!(
            !aggregate_column.data_type.is_array(),
            "array columns cannot be aggregated"
        );
        if *function != Aggregate::Count {
            ensure!(
                matches!(
                    aggregate_column.data_type,
                    ColumnType::Integer | ColumnType::Real
                ),
                "numeric aggregate requires an integer or real column"
            );
        }
        let request_name = match function {
            Aggregate::Count => "value_count",
            Aggregate::Sum => "sum",
            Aggregate::Average => "avg",
            Aggregate::Min => "min",
            Aggregate::Max => "max",
        };
        let name = format!("__aq_metric_{index}");
        requests.insert(
            name.clone(),
            json!({ request_name: { "field": aggregation_field(aggregate_column) } }),
        );
        metrics.push(TypedMetric {
            projection_index: index,
            name,
            function: *function,
        });
    }
    ensure!(
        !metrics.is_empty(),
        "aggregation requires at least one metric"
    );
    Ok((metrics, requests))
}

fn column_by_name<'a>(def: &'a TableDef, name: &str) -> Result<&'a ColumnDef> {
    column(def, name)
}

fn aggregation_request(
    groups: &[TypedGroup<'_>],
    metrics: serde_json::Map<String, Value>,
    bucket_size: usize,
) -> Result<Aggregations> {
    let mut request = Value::Object(metrics);
    for group in groups.iter().rev() {
        request = json!({
            group.name.clone(): {
                "terms": {
                    "field": aggregation_field(group.column),
                    "size": bucket_size,
                    "missing": NULL_SENTINEL
                },
                "aggs": request
            }
        });
    }
    Ok(serde_json::from_value(request)?)
}

fn typed_bucket_size(request: &ReadRequest, order: &[OrderSpec], groups: usize) -> usize {
    const MAX_BUCKETS: usize = 65_000;
    let requested = request
        .limit
        .saturating_add(request.offset)
        .clamp(1, MAX_BUCKETS);
    if groups != 1 || order.len() != 1 || order[0].asc {
        return MAX_BUCKETS;
    }
    let ordered_by_count = request.projection.iter().any(|item| {
        matches!(item, Projection::Aggregate { function: Aggregate::Count, alias, .. } if alias.eq_ignore_ascii_case(&order[0].key))
    });
    if ordered_by_count {
        requested
    } else {
        MAX_BUCKETS
    }
}

fn collect_typed_group_rows(
    request: &ReadRequest,
    groups: &[TypedGroup<'_>],
    metrics: &[TypedMetric],
    level: usize,
    node: &Value,
    keys: &mut Vec<Value>,
    rows: &mut Vec<Vec<Value>>,
) -> Result<()> {
    let buckets = node
        .get(&groups[level].name)
        .and_then(|value| value.get("buckets"))
        .and_then(Value::as_array)
        .context("invalid Tantivy aggregation response")?;
    for bucket in buckets {
        let key = bucket.get("key").cloned().context("missing group key")?;
        keys.push(normalize_group_key(groups[level].column, key)?);
        if level + 1 == groups.len() {
            rows.push(typed_aggregation_row(
                request, groups, metrics, keys, bucket,
            )?);
        } else {
            collect_typed_group_rows(request, groups, metrics, level + 1, bucket, keys, rows)?;
        }
        keys.pop();
    }
    Ok(())
}

fn typed_aggregation_row(
    request: &ReadRequest,
    groups: &[TypedGroup<'_>],
    metrics: &[TypedMetric],
    keys: &[Value],
    node: &Value,
) -> Result<Vec<Value>> {
    request
        .projection
        .iter()
        .enumerate()
        .map(|(index, item)| match item {
            Projection::Column { column, .. } => {
                let key_index = groups
                    .iter()
                    .position(|group| group.column.name.eq_ignore_ascii_case(column))
                    .context("projection is not a group key")?;
                Ok(keys[key_index].clone())
            }
            Projection::Aggregate { .. } => {
                let metric = metrics
                    .iter()
                    .find(|metric| metric.projection_index == index)
                    .context("missing aggregation metric")?;
                let value = node
                    .get(&metric.name)
                    .and_then(|value| value.get("value"))
                    .cloned()
                    .unwrap_or(Value::Null);
                if metric.function == Aggregate::Count {
                    Ok(json!(value.as_f64().unwrap_or(0.0) as u64))
                } else {
                    Ok(value)
                }
            }
            Projection::Score { .. } => bail!("score cannot be aggregated"),
            Projection::Highlight { .. } => bail!("highlight cannot be aggregated"),
        })
        .collect()
}

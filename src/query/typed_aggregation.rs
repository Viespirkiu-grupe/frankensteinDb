use super::*;
use crate::aggregation_api::collect_aggregation_results;
use tantivy::aggregation::Key;
use tantivy::aggregation::agg_result::{
    AggregationResult, BucketEntry, BucketResult, MetricResult,
};

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

struct TypedBucketPlan {
    size: usize,
    order: Option<(String, Order)>,
}

pub(crate) fn execute_typed_aggregation(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    index: &Index,
    request: &ReadRequest,
    order: &[OrderSpec],
    pool: &rayon::ThreadPool,
) -> Result<QueryResult> {
    ensure!(
        !request.projection.is_empty(),
        "aggregation requires an explicit projection"
    );
    let groups = typed_groups(def, request)?;
    validate_group_projection(request, &groups)?;
    let (metrics, metric_requests) = typed_metrics(def, request)?;
    let bucket_plan = typed_bucket_plan(request, order, &groups, &metrics);
    let aggregations = aggregation_request(&groups, metric_requests, &bucket_plan)?;
    let results = collect_aggregation_results(searcher, query, index, aggregations, pool)?;
    let mut rows = if groups.is_empty() {
        vec![typed_aggregation_row(
            request,
            &groups,
            &metrics,
            &[],
            &results,
        )?]
    } else {
        let mut rows = Vec::new();
        collect_typed_group_rows(
            request,
            &groups,
            &metrics,
            0,
            &results,
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
            ensure!(
                column.data_type != ColumnType::GeoPoint,
                "geo columns cannot be grouped; use geo_tile_grid aggregation"
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
            Projection::GeoDistance { .. } => {
                bail!("geo distance cannot be selected with tabular aggregation")
            }
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
    plan: &TypedBucketPlan,
) -> Result<Aggregations> {
    let mut request = Value::Object(metrics);
    for group in groups.iter().rev() {
        let mut terms = json!({
            "field": aggregation_field(group.column),
            "size": plan.size,
            "missing": NULL_SENTINEL
        });
        if let Some((target, order)) = &plan.order {
            terms.as_object_mut().expect("terms object").insert(
                "order".into(),
                json!({target: if *order == Order::Asc { "asc" } else { "desc" }}),
            );
        }
        request = json!({
            group.name.clone(): {
                "terms": terms,
                "aggs": request
            }
        });
    }
    Ok(serde_json::from_value(request)?)
}

fn typed_bucket_plan(
    request: &ReadRequest,
    order: &[OrderSpec],
    groups: &[TypedGroup<'_>],
    metrics: &[TypedMetric],
) -> TypedBucketPlan {
    const MAX_BUCKETS: usize = 65_000;
    let requested = request
        .limit
        .saturating_add(request.offset)
        .clamp(1, MAX_BUCKETS);
    if order.is_empty() {
        return TypedBucketPlan {
            size: requested,
            order: None,
        };
    }
    if groups.len() == 1 && order.len() == 1 {
        let spec = &order[0];
        let target = request
            .projection
            .iter()
            .enumerate()
            .find_map(|(index, item)| {
                if !typed_projection_name(item).eq_ignore_ascii_case(&spec.key) {
                    return None;
                }
                match item {
                    Projection::Column { .. } if !groups[0].column.nullable => {
                        Some("_key".to_owned())
                    }
                    Projection::Aggregate { .. } => metrics
                        .iter()
                        .find(|metric| metric.projection_index == index)
                        .map(|metric| metric.name.clone()),
                    _ => None,
                }
            });
        if let Some(target) = target {
            return TypedBucketPlan {
                size: requested,
                order: Some((target, if spec.asc { Order::Asc } else { Order::Desc })),
            };
        }
    }
    TypedBucketPlan {
        size: MAX_BUCKETS,
        order: None,
    }
}

fn collect_typed_group_rows(
    request: &ReadRequest,
    groups: &[TypedGroup<'_>],
    metrics: &[TypedMetric],
    level: usize,
    node: &AggregationResults,
    keys: &mut Vec<Value>,
    rows: &mut Vec<Vec<Value>>,
) -> Result<()> {
    let buckets = terms_buckets(node, &groups[level].name)?;
    for bucket in buckets {
        keys.push(normalize_group_key(
            groups[level].column,
            aggregation_bucket_value(groups[level].column, bucket)?,
        )?);
        if level + 1 == groups.len() {
            rows.push(typed_aggregation_row(
                request,
                groups,
                metrics,
                keys,
                &bucket.sub_aggregation,
            )?);
        } else {
            collect_typed_group_rows(
                request,
                groups,
                metrics,
                level + 1,
                &bucket.sub_aggregation,
                keys,
                rows,
            )?;
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
    node: &AggregationResults,
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
                let value = metric_value(node, &metric.name);
                if metric.function == Aggregate::Count {
                    Ok(json!(value.unwrap_or(0.0) as u64))
                } else {
                    Ok(value
                        .and_then(Number::from_f64)
                        .map(Value::Number)
                        .unwrap_or(Value::Null))
                }
            }
            Projection::Score { .. } => bail!("score cannot be aggregated"),
            Projection::Highlight { .. } => bail!("highlight cannot be aggregated"),
            Projection::GeoDistance { .. } => bail!("geo distance cannot be aggregated"),
        })
        .collect()
}

fn terms_buckets<'a>(results: &'a AggregationResults, name: &str) -> Result<&'a [BucketEntry]> {
    match results.0.get(name) {
        Some(AggregationResult::BucketResult(BucketResult::Terms { buckets, .. })) => Ok(buckets),
        _ => bail!("invalid Tantivy terms aggregation response: {name}"),
    }
}

fn aggregation_key_value(key: &Key) -> Value {
    match key {
        Key::Str(value) => Value::String(value.clone()),
        Key::I64(value) => json!(value),
        Key::U64(value) => json!(value),
        Key::F64(value) => Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    }
}

fn aggregation_bucket_value(column: &ColumnDef, bucket: &BucketEntry) -> Result<Value> {
    if matches!(
        column.data_type,
        ColumnType::Date | ColumnType::DateTime | ColumnType::Timestamp
    ) {
        return Ok(Value::String(
            bucket
                .key_as_string
                .clone()
                .context("date aggregation bucket has no formatted key")?,
        ));
    }
    Ok(aggregation_key_value(&bucket.key))
}

fn metric_value(results: &AggregationResults, name: &str) -> Option<f64> {
    let AggregationResult::MetricResult(metric) = results.0.get(name)? else {
        return None;
    };
    match metric {
        MetricResult::Average(value)
        | MetricResult::Count(value)
        | MetricResult::Max(value)
        | MetricResult::Min(value)
        | MetricResult::Sum(value)
        | MetricResult::Cardinality(value) => value.value,
        MetricResult::Stats(_)
        | MetricResult::ExtendedStats(_)
        | MetricResult::Percentiles(_)
        | MetricResult::TopHits(_) => None,
    }
}

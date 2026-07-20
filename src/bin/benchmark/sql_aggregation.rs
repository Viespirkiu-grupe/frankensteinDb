use std::collections::BTreeMap;

use frankensteindb::{Aggregation, AggregationRange, CompositeSource, HistogramBounds, Metric};

use crate::sql_display::{filter_sql, sql_value};

pub(crate) fn aggregation_expression(aggregation: &Aggregation) -> String {
    match aggregation {
        Aggregation::Terms {
            column,
            size,
            segment_size,
            min_doc_count,
            missing,
            order,
            aggregations,
        } => format!(
            "TERMS({column}, size => {size}, segment_size => {}, min_doc_count => {}, missing => {}, order => {}{})",
            optional_display(*segment_size),
            optional_display(*min_doc_count),
            missing
                .as_ref()
                .map(sql_value)
                .unwrap_or_else(|| "NONE".into()),
            order
                .as_ref()
                .map(|order| format!("{} {}", order.target, direction(order.descending)))
                .unwrap_or_else(|| "DEFAULT".into()),
            children_sql(aggregations)
        ),
        Aggregation::Histogram {
            column,
            interval,
            offset,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => format!(
            "HISTOGRAM({column}, interval => {interval}, offset => {}, min_doc_count => {min_doc_count}, hard_bounds => {}, extended_bounds => {}, keyed => {keyed}{})",
            optional_display(*offset),
            bounds_sql(hard_bounds.as_ref()),
            bounds_sql(extended_bounds.as_ref()),
            children_sql(aggregations)
        ),
        Aggregation::DateHistogram {
            column,
            fixed_interval,
            offset,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => format!(
            "DATE_HISTOGRAM({column}, fixed_interval => {fixed_interval}, offset => {}, min_doc_count => {min_doc_count}, hard_bounds => {}, extended_bounds => {}, keyed => {keyed}{})",
            offset.as_deref().unwrap_or("NONE"),
            bounds_sql(hard_bounds.as_ref()),
            bounds_sql(extended_bounds.as_ref()),
            children_sql(aggregations)
        ),
        Aggregation::Range {
            column,
            ranges,
            keyed,
            aggregations,
        } => format!(
            "RANGES({column}, {}, keyed => {keyed}{})",
            ranges.iter().map(range_sql).collect::<Vec<_>>().join(", "),
            children_sql(aggregations)
        ),
        Aggregation::JsonTerms {
            target,
            size,
            missing,
            order,
            aggregations,
        } => format!(
            "TERMS({}.{}, type => {:?}, size => {size}, missing => {}, order => {}{})",
            target.column,
            target.path,
            target.data_type,
            missing
                .as_ref()
                .map(sql_value)
                .unwrap_or_else(|| "NONE".into()),
            order
                .as_ref()
                .map(|order| format!("{} {}", order.target, direction(order.descending)))
                .unwrap_or_else(|| "DEFAULT".into()),
            children_sql(aggregations)
        ),
        Aggregation::JsonHistogram {
            target,
            interval,
            min_doc_count,
            hard_bounds,
            extended_bounds,
            keyed,
            aggregations,
        } => format!(
            "HISTOGRAM({}.{}, type => {:?}, interval => {interval}, min_doc_count => {min_doc_count}, hard_bounds => {}, extended_bounds => {}, keyed => {keyed}{})",
            target.column,
            target.path,
            target.data_type,
            bounds_sql(hard_bounds.as_ref()),
            bounds_sql(extended_bounds.as_ref()),
            children_sql(aggregations)
        ),
        Aggregation::JsonRange {
            target,
            ranges,
            keyed,
            aggregations,
        } => format!(
            "RANGES({}.{}, type => {:?}, {}, keyed => {keyed}{})",
            target.column,
            target.path,
            target.data_type,
            ranges.iter().map(range_sql).collect::<Vec<_>>().join(", "),
            children_sql(aggregations)
        ),
        Aggregation::Filter {
            filter,
            aggregations,
        } => format!(
            "FILTER({}{})",
            filter_sql(filter),
            children_sql(aggregations)
        ),
        Aggregation::Composite {
            sources,
            size,
            after,
            aggregations,
        } => {
            let after = if after.is_empty() {
                String::new()
            } else {
                ", AFTER => CURSOR".into()
            };
            format!(
                "COMPOSITE({}, SIZE => {size}{after}{})",
                sources
                    .iter()
                    .map(composite_source_sql)
                    .collect::<Vec<_>>()
                    .join(", "),
                children_sql(aggregations)
            )
        }
        Aggregation::Metric {
            function,
            column,
            json_path,
            percents,
            missing,
        } => format!(
            "{}({}, percents => {}, missing => {})",
            metric_name(*function),
            json_path
                .as_ref()
                .map(|path| format!("{}.{}", path.column, path.path))
                .or_else(|| column.clone())
                .unwrap_or_else(|| "*".into()),
            percents
                .as_ref()
                .map(|values| format!("{values:?}"))
                .unwrap_or_else(|| "DEFAULT".into()),
            missing
                .as_ref()
                .map(sql_value)
                .unwrap_or_else(|| "NONE".into())
        ),
        Aggregation::TopHits {
            size,
            sort,
            columns,
        } => format!(
            "TOP_HITS(size => {size}, columns => [{}], order_by => [{}])",
            columns.join(", "),
            sort.iter()
                .map(|sort| format!("{} {}", sort.column, direction(sort.descending)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn children_sql(aggregations: &BTreeMap<String, Aggregation>) -> String {
    if aggregations.is_empty() {
        String::new()
    } else {
        format!(
            ", sub_aggregations => [{}]",
            aggregations
                .iter()
                .map(|(name, aggregation)| format!(
                    "{} AS {name}",
                    aggregation_expression(aggregation)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn bounds_sql(bounds: Option<&HistogramBounds>) -> String {
    bounds
        .map(|bounds| format!("{}..{}", sql_value(&bounds.min), sql_value(&bounds.max)))
        .unwrap_or_else(|| "NONE".into())
}

fn range_sql(range: &AggregationRange) -> String {
    format!(
        "{}:{}..{}",
        range.key.as_deref().unwrap_or("unnamed"),
        range
            .from
            .as_ref()
            .map(sql_value)
            .unwrap_or_else(|| "-INF".into()),
        range
            .to
            .as_ref()
            .map(sql_value)
            .unwrap_or_else(|| "+INF".into())
    )
}

fn composite_source_sql(source: &CompositeSource) -> String {
    match source {
        CompositeSource::Terms {
            name,
            column,
            descending,
            missing_bucket,
            missing_order,
        } => format!(
            "TERMS({column}) AS {name} {}{}",
            direction(*descending),
            composite_nulls_sql(*missing_bucket, *missing_order)
        ),
        CompositeSource::Histogram {
            name,
            column,
            interval,
            descending,
            missing_bucket,
            missing_order,
        } => format!(
            "HISTOGRAM({column}, INTERVAL => {interval}) AS {name} {}{}",
            direction(*descending),
            composite_nulls_sql(*missing_bucket, *missing_order)
        ),
        CompositeSource::DateHistogram {
            name,
            column,
            fixed_interval,
            calendar_interval,
            descending,
            missing_bucket,
            missing_order,
        } => {
            let interval = fixed_interval
                .as_ref()
                .map(|interval| format!("FIXED_INTERVAL => '{interval}'"))
                .or_else(|| {
                    calendar_interval.as_ref().map(|interval| {
                        format!(
                            "CALENDAR_INTERVAL => {}",
                            format!("{interval:?}").to_uppercase()
                        )
                    })
                })
                .unwrap_or_else(|| "INTERVAL => INVALID".into());
            format!(
                "DATE_HISTOGRAM({column}, {interval}) AS {name} {}{}",
                direction(*descending),
                composite_nulls_sql(*missing_bucket, *missing_order)
            )
        }
        CompositeSource::JsonTerms {
            name,
            target,
            descending,
            missing_bucket,
            missing_order,
        } => format!(
            "TERMS({}.{}, TYPE => {:?}) AS {name} {}{}",
            target.column,
            target.path,
            target.data_type,
            direction(*descending),
            composite_nulls_sql(*missing_bucket, *missing_order)
        ),
    }
}

fn composite_nulls_sql(
    missing_bucket: bool,
    missing_order: frankensteindb::MissingOrder,
) -> String {
    if !missing_bucket {
        return " EXCLUDE NULLS".into();
    }
    format!(" NULLS {}", format!("{missing_order:?}").to_uppercase())
}

fn optional_display(value: Option<impl std::fmt::Display>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "DEFAULT".into())
}

const fn direction(descending: bool) -> &'static str {
    if descending { "DESC" } else { "ASC" }
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Count => "COUNT",
        Metric::Sum => "SUM",
        Metric::Average => "AVG",
        Metric::Min => "MIN",
        Metric::Max => "MAX",
        Metric::Cardinality => "CARDINALITY",
        Metric::Percentiles => "PERCENTILES",
        Metric::Stats => "STATS",
        Metric::ExtendedStats => "EXTENDED_STATS",
    }
}

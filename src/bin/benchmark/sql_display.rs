use std::collections::BTreeMap;

use frankensteindb::{Aggregation, Comparison, Filter, Mutation, Projection, ReadRequest};
use serde_json::Value;

use crate::sql_aggregation::aggregation_expression;

pub(crate) fn read_sql(request: &ReadRequest) -> String {
    let projection = if request.projection.is_empty() {
        "*".into()
    } else {
        request
            .projection
            .iter()
            .map(projection_sql)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut sql = format!("SELECT {projection}\nFROM {}", request.table);
    if let Some(filter) = &request.filter {
        sql.push_str(&format!("\nWHERE {}", filter_sql(filter)));
    }
    if !request.group_by.is_empty() {
        sql.push_str(&format!("\nGROUP BY {}", request.group_by.join(", ")));
    }
    if !request.order_by.is_empty() {
        let order = request
            .order_by
            .iter()
            .map(|sort| {
                let field = sort
                    .json_path
                    .as_ref()
                    .map(|path| format!("{}.{path}", sort.column))
                    .unwrap_or_else(|| sort.column.clone());
                format!("{field} {}", if sort.descending { "DESC" } else { "ASC" })
            })
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!("\nORDER BY {order}"));
    }
    sql.push_str(&format!("\nLIMIT {}", request.limit));
    if request.offset > 0 {
        sql.push_str(&format!(" OFFSET {}", request.offset));
    }
    sql.push(';');
    sql
}

pub(crate) fn mutation_sql(mutation: &Mutation) -> String {
    match mutation {
        Mutation::Insert { table, row } => format!(
            "INSERT INTO {table} ({})\nVALUES ({});",
            row.keys().cloned().collect::<Vec<_>>().join(", "),
            row.values().map(sql_value).collect::<Vec<_>>().join(", ")
        ),
        Mutation::Update {
            table,
            values,
            filter,
        } => format!(
            "UPDATE {table}\nSET {}\nWHERE {};",
            values
                .iter()
                .map(|(column, value)| format!("{column} = {}", sql_value(value)))
                .collect::<Vec<_>>()
                .join(", "),
            filter_sql(filter)
        ),
        Mutation::Delete { table, filter } => {
            format!("DELETE FROM {table}\nWHERE {};", filter_sql(filter))
        }
    }
}

pub(crate) fn aggregation_sql(
    table: &str,
    filter: Option<&Filter>,
    aggregations: &BTreeMap<String, Aggregation>,
) -> String {
    let expressions = aggregations
        .iter()
        .map(|(name, aggregation)| format!("{} AS {name}", aggregation_expression(aggregation)))
        .collect::<Vec<_>>()
        .join(",\n  ");
    let mut sql = format!("SELECT\n  {expressions}\nFROM {table}");
    if let Some(filter) = filter {
        sql.push_str(&format!("\nWHERE {}", filter_sql(filter)));
    }
    sql.push(';');
    sql
}

fn projection_sql(projection: &Projection) -> String {
    match projection {
        Projection::Column { column, alias } => alias
            .as_ref()
            .map(|alias| format!("{column} AS {alias}"))
            .unwrap_or_else(|| column.clone()),
        Projection::Score { alias } => alias
            .as_ref()
            .map(|alias| format!("SCORE() AS {alias}"))
            .unwrap_or_else(|| "SCORE()".into()),
        Projection::Highlight {
            column,
            alias,
            fragment_size,
        } => format!(
            "HIGHLIGHT({column}, {fragment_size}){}",
            alias
                .as_ref()
                .map(|alias| format!(" AS {alias}"))
                .unwrap_or_default()
        ),
        Projection::GeoDistance {
            column,
            from,
            mode,
            alias,
        } => format!(
            "GEO_DISTANCE({column}, {}, {}, {mode:?}){}",
            from.lat,
            from.lon,
            alias
                .as_ref()
                .map(|alias| format!(" AS {alias}"))
                .unwrap_or_default()
        ),
        Projection::Aggregate {
            function,
            column,
            alias,
        } => format!(
            "{:?}({}) AS {alias}",
            function,
            column.as_deref().unwrap_or("*")
        )
        .to_uppercase(),
    }
}

pub(crate) fn filter_sql(filter: &Filter) -> String {
    match filter {
        Filter::Compare {
            column,
            operator,
            value,
        } => format!(
            "{column} {} {}",
            comparison_sql(*operator),
            sql_value(value)
        ),
        Filter::Between {
            column,
            lower,
            upper,
        } => format!(
            "{column} BETWEEN {} AND {}",
            sql_value(lower),
            sql_value(upper)
        ),
        Filter::In { column, values } => format!(
            "{column} IN ({})",
            values.iter().map(sql_value).collect::<Vec<_>>().join(", ")
        ),
        Filter::IsNull { column, negated } => {
            format!("{column} IS {}NULL", if *negated { "NOT " } else { "" })
        }
        Filter::Search { fields, query } => format!(
            "SEARCH({}, {})",
            if fields.is_empty() {
                "*".into()
            } else {
                fields.join(" | ")
            },
            sql_string(query)
        ),
        Filter::SearchBoosted { fields, query, .. } => format!(
            "SEARCH_BOOSTED({}, {})",
            fields
                .iter()
                .map(|(field, boost)| format!("{field}^{boost}"))
                .collect::<Vec<_>>()
                .join(" | "),
            sql_string(query)
        ),
        Filter::Fuzzy {
            column,
            value,
            distance,
            ..
        } => format!("FUZZY({column}, {}, {distance})", sql_string(value)),
        Filter::Prefix { column, value } => {
            format!("PREFIX({column}, {})", sql_string(value))
        }
        Filter::PhrasePrefix { column, phrase, .. } => {
            format!("PHRASE_PREFIX({column}, {})", sql_string(phrase))
        }
        Filter::DisjunctionMax { fields, query, .. } => format!(
            "DISMAX({}, {})",
            fields.keys().cloned().collect::<Vec<_>>().join(" | "),
            sql_string(query)
        ),
        Filter::Regex { column, pattern } => {
            format!("REGEX({column}, {})", sql_string(pattern))
        }
        Filter::RegexPhrase {
            column, patterns, ..
        } => format!("REGEX_PHRASE({column}, {})", patterns.join(" | ")),
        Filter::JsonSearch {
            column,
            path,
            query,
        } => format!("SEARCH({column}.{path}, {})", sql_string(query)),
        Filter::JsonCompare {
            column,
            path,
            operator,
            value,
            ..
        } => format!(
            "{column}.{path} {} {}",
            comparison_sql(*operator),
            sql_value(value)
        ),
        Filter::JsonBetween {
            column,
            path,
            lower,
            upper,
            ..
        } => format!(
            "{column}.{path} BETWEEN {} AND {}",
            sql_value(lower),
            sql_value(upper)
        ),
        Filter::JsonExists {
            column,
            path,
            negated,
            ..
        } => format!(
            "JSON_EXISTS({column}.{path}){}",
            if *negated { " = FALSE" } else { "" }
        ),
        Filter::GeoDistance {
            column,
            center,
            radius_meters,
        } => format!(
            "GEO_DISTANCE({column}, {}, {}) <= {radius_meters}",
            center.lat, center.lon
        ),
        Filter::GeoBoundingBox { column, bounds } => format!(
            "GEO_BOUNDING_BOX({column}, {}, {}, {}, {})",
            bounds.top_left.lat,
            bounds.top_left.lon,
            bounds.bottom_right.lat,
            bounds.bottom_right.lon
        ),
        Filter::GeoDistanceCompare {
            column,
            center,
            mode,
            operator,
            distance_meters,
        } => format!(
            "GEO_DISTANCE({column}, {}, {}, {mode:?}) {} {distance_meters}",
            center.lat,
            center.lon,
            comparison_sql(*operator)
        ),
        Filter::All { filters } => joined_filters(filters, "AND"),
        Filter::Any { filters } => joined_filters(filters, "OR"),
        Filter::Not { filter } => format!("NOT ({})", filter_sql(filter)),
    }
}

fn joined_filters(filters: &[Filter], operator: &str) -> String {
    format!(
        "({})",
        filters
            .iter()
            .map(filter_sql)
            .collect::<Vec<_>>()
            .join(&format!(" {operator} "))
    )
}

fn comparison_sql(comparison: Comparison) -> &'static str {
    match comparison {
        Comparison::Equal => "=",
        Comparison::NotEqual => "!=",
        Comparison::Greater => ">",
        Comparison::GreaterOrEqual => ">=",
        Comparison::Less => "<",
        Comparison::LessOrEqual => "<=",
    }
}

pub(crate) fn sql_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(value) => sql_string(value),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(sql_value).collect::<Vec<_>>().join(", ")
        ),
        _ => value.to_string(),
    }
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

use frankensteindb::{
    Aggregate, Comparison, Filter, GeoDistanceMode, Projection, ReadRequest, Sort,
};
use serde_json::json;

use super::*;

pub(crate) fn run_benchmark_suite(
    database: &mut Database,
    first_id: i64,
    first_row: &[Value],
    iterations: usize,
    progress: &ProgressReporter,
    mut capture: Option<&mut BenchmarkCapture>,
) -> Result<Vec<Measurement>> {
    let reads = benchmark_reads(first_id, first_row);
    let benchmark_count = reads.len() + aggregation_benchmark_count() + 3;
    let mut benchmarks = Vec::new();
    for (index, (name, request)) in reads.into_iter().enumerate() {
        progress.benchmark_start(index + 1, benchmark_count, name, iterations);
        let measurement =
            measure_read(database, name, &request, iterations, capture.as_deref_mut())?;
        progress.benchmark_done(&measurement);
        benchmarks.push(measurement);
    }
    benchmarks.extend(run_aggregation_benchmarks(
        database,
        iterations,
        progress,
        benchmarks.len() + 1,
        benchmark_count,
        capture.as_deref_mut(),
    )?);
    let offset = benchmark_count - 2;
    progress.benchmark_start(offset, benchmark_count, "single_row_update", iterations);
    let measurement = measure_updates(database, first_id, iterations, capture.as_deref_mut())?;
    progress.benchmark_done(&measurement);
    benchmarks.push(measurement);
    progress.benchmark_start(
        offset + 1,
        benchmark_count,
        "selective_non_pk_update",
        iterations,
    );
    let measurement =
        measure_selective_updates(database, first_id, iterations, capture.as_deref_mut())?;
    progress.benchmark_done(&measurement);
    benchmarks.push(measurement);
    progress.benchmark_start(
        offset + 2,
        benchmark_count,
        "selective_non_pk_delete",
        iterations,
    );
    let measurement = measure_selective_deletes(database, first_row, iterations, capture)?;
    progress.benchmark_done(&measurement);
    benchmarks.push(measurement);
    Ok(benchmarks)
}

fn benchmark_reads(first_id: i64, first_row: &[Value]) -> Vec<(&'static str, ReadRequest)> {
    let supplier = first_row[13]
        .as_array()
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or(json!(""));
    let cpv = first_row[17]
        .as_array()
        .and_then(|values| values.first())
        .cloned()
        .unwrap_or(json!(0));
    let organization = first_row[7].clone();
    vec![
        (
            "primary_key_lookup",
            read(
                vec![],
                Some(eq("unikalusId", json!(first_id))),
                vec![],
                vec![],
                100,
            ),
        ),
        (
            "full_text_top_20",
            read(
                columns(&["unikalusId", "pavadinimas", "_score"]),
                Some(search(&["pavadinimas"], "paslaugos")),
                vec![],
                score_order(),
                20,
            ),
        ),
        (
            "search_filter_top_20",
            read(
                columns(&["unikalusId", "pavadinimas", "numatomaVerte", "_score"]),
                Some(all(vec![
                    search(&[], "paslaugos"),
                    between("numatomaVerte", json!(1000), json!(100000)),
                    eq("istrinta", json!(false)),
                ])),
                vec![],
                score_order(),
                20,
            ),
        ),
        (
            "typed_date_range",
            read(
                columns(&["unikalusId", "pavadinimas", "galiojimoData"]),
                Some(between(
                    "galiojimoData",
                    json!("2024-01-01"),
                    json!("2024-12-31"),
                )),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "typed_timestamp_range",
            read(
                columns(&["unikalusId", "redagavimoData"]),
                Some(between(
                    "redagavimoData",
                    json!("2024-01-01T00:00:00.000"),
                    json!("2024-12-31T23:59:59.999"),
                )),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "supplier_array_filter",
            read(
                columns(&["unikalusId", "tiekejuKodai"]),
                Some(eq("tiekejuKodai", supplier)),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "cpv_array_filter",
            read(
                columns(&["unikalusId", "bvpzKodai"]),
                Some(eq("bvpzKodai", cpv.clone())),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "supplier_name_search",
            read(
                columns(&["unikalusId", "tiekejuPavadinimai", "_score"]),
                Some(search(&["tiekejuPavadinimai"], "uab")),
                vec![],
                score_order(),
                20,
            ),
        ),
        (
            "organization_code_filter",
            read(
                columns(&["unikalusId", "perkanciosiosOrganizacijosPavadinimas"]),
                Some(eq("perkanciosiosOrganizacijosKodas", organization)),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "null_filter_top_20",
            read(
                columns(&["unikalusId", "pavadinimas"]),
                Some(Filter::IsNull {
                    column: "pirkimoNumeris".into(),
                    negated: false,
                }),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "boolean_filter_top_20",
            read(
                columns(&["unikalusId", "pavadinimas"]),
                Some(all(vec![
                    eq("istrinta", json!(false)),
                    eq("pakeitimas", json!(false)),
                ])),
                vec![],
                vec![],
                20,
            ),
        ),
        (
            "search_cpv_filter_top_20",
            read(
                columns(&["unikalusId", "pavadinimas", "_score"]),
                Some(all(vec![
                    search(&["pavadinimas"], "paslaugos"),
                    eq("bvpzKodai", cpv),
                ])),
                vec![],
                score_order(),
                20,
            ),
        ),
        (
            "primary_key_sorted_page",
            read(
                columns(&["unikalusId", "pavadinimas"]),
                None,
                vec![],
                vec![Sort {
                    column: "unikalusId".into(),
                    json_path: None,
                    json_type: None,
                    descending: true,
                    geo_distance_from: None,
                    geo_distance_mode: GeoDistanceMode::Min,
                }],
                20,
            ),
        ),
        (
            "count_all",
            aggregation(
                &[],
                vec![metric(Aggregate::Count, None, "contracts")],
                None,
                100,
            ),
        ),
        (
            "filtered_count",
            aggregation(
                &[],
                vec![metric(Aggregate::Count, None, "contracts")],
                Some(all(vec![
                    eq("istrinta", json!(false)),
                    compare("numatomaVerte", Comparison::GreaterOrEqual, json!(10000)),
                ])),
                100,
            ),
        ),
        (
            "numeric_metrics",
            aggregation(
                &[],
                vec![
                    metric(Aggregate::Count, Some("numatomaVerte"), "valued"),
                    metric(Aggregate::Sum, Some("numatomaVerte"), "total"),
                    metric(Aggregate::Average, Some("numatomaVerte"), "average"),
                    metric(Aggregate::Min, Some("numatomaVerte"), "minimum"),
                    metric(Aggregate::Max, Some("numatomaVerte"), "maximum"),
                ],
                Some(eq("istrinta", json!(false))),
                100,
            ),
        ),
        (
            "grouped_aggregation",
            grouped(
                &["tipas"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Average, Some("numatomaVerte"), "average_value"),
                ],
                Some(eq("istrinta", json!(false))),
                "contracts",
                100,
            ),
        ),
        (
            "boolean_grouped_aggregation",
            grouped(
                &["istrinta"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Average, Some("numatomaVerte"), "average_value"),
                ],
                None,
                "contracts",
                100,
            ),
        ),
        (
            "multi_column_grouped_aggregation",
            grouped(
                &["tipas", "istrinta"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Sum, Some("numatomaVerte"), "total_value"),
                ],
                None,
                "contracts",
                100,
            ),
        ),
        (
            "search_grouped_aggregation",
            grouped(
                &["tipas"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Average, Some("numatomaVerte"), "average_value"),
                ],
                Some(search(&[], "paslaugos")),
                "contracts",
                100,
            ),
        ),
        (
            "organization_top_20_aggregation",
            grouped(
                &["perkanciosiosOrganizacijosKodas"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Sum, Some("numatomaVerte"), "total_value"),
                ],
                None,
                "contracts",
                20,
            ),
        ),
        (
            "category_top_20_aggregation",
            grouped(
                &["kategorija"],
                vec![
                    metric(Aggregate::Count, None, "contracts"),
                    metric(Aggregate::Average, Some("numatomaVerte"), "average_value"),
                ],
                None,
                "contracts",
                20,
            ),
        ),
    ]
}

fn read(
    projection: Vec<Projection>,
    filter: Option<Filter>,
    group_by: Vec<String>,
    order_by: Vec<Sort>,
    limit: usize,
) -> ReadRequest {
    ReadRequest {
        table: "sutartys".into(),
        projection,
        filter,
        group_by,
        order_by,
        limit,
        offset: 0,
        search_after: None,
        min_score: None,
    }
}
fn columns(names: &[&str]) -> Vec<Projection> {
    names
        .iter()
        .map(|name| {
            if *name == "_score" {
                Projection::Score { alias: None }
            } else {
                Projection::Column {
                    column: (*name).into(),
                    alias: None,
                }
            }
        })
        .collect()
}
fn score_order() -> Vec<Sort> {
    vec![Sort {
        column: "_score".into(),
        json_path: None,
        json_type: None,
        descending: true,
        geo_distance_from: None,
        geo_distance_mode: GeoDistanceMode::Min,
    }]
}
fn compare(column: &str, operator: Comparison, value: Value) -> Filter {
    Filter::Compare {
        column: column.into(),
        operator,
        value,
    }
}
fn eq(column: &str, value: Value) -> Filter {
    compare(column, Comparison::Equal, value)
}
fn between(column: &str, lower: Value, upper: Value) -> Filter {
    Filter::Between {
        column: column.into(),
        lower,
        upper,
    }
}
fn search(fields: &[&str], query: &str) -> Filter {
    Filter::Search {
        fields: fields.iter().map(|field| (*field).into()).collect(),
        query: query.into(),
    }
}
fn all(filters: Vec<Filter>) -> Filter {
    Filter::All { filters }
}
fn metric(function: Aggregate, column: Option<&str>, alias: &str) -> Projection {
    Projection::Aggregate {
        function,
        column: column.map(Into::into),
        alias: alias.into(),
    }
}
fn aggregation(
    groups: &[&str],
    mut metrics: Vec<Projection>,
    filter: Option<Filter>,
    limit: usize,
) -> ReadRequest {
    let mut projection = columns(groups);
    projection.append(&mut metrics);
    read(
        projection,
        filter,
        groups.iter().map(|group| (*group).into()).collect(),
        vec![],
        limit,
    )
}
fn grouped(
    groups: &[&str],
    metrics: Vec<Projection>,
    filter: Option<Filter>,
    order: &str,
    limit: usize,
) -> ReadRequest {
    let mut request = aggregation(groups, metrics, filter, limit);
    request.order_by.push(Sort {
        column: order.into(),
        json_path: None,
        json_type: None,
        descending: true,
        geo_distance_from: None,
        geo_distance_mode: GeoDistanceMode::Min,
    });
    request
}
